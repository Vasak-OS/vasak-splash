//! El lazo del splash, aislado detrás de rasgos para poder probarlo entero sin
//! hardware: el backend de DRM, el reloj y la fuente de órdenes son rasgos que
//! en producción implementa el binario real y en las pruebas unos dobles.
//!
//! Invariantes anti-parpadeo que este lazo sostiene (ver el diseño):
//!  - se toma el DRM (SET_MASTER) antes de dibujar y se suelta (DROP_MASTER)
//!    recién en el handoff o en el timeout;
//!  - el primer `present` es sobre un buffer ya dibujado, nunca un modeset en
//!    negro;
//!  - nadie hace DPMS-off ni un modeset nulo en el medio.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::control::Command;
use crate::{geometry, logo, render};

/// Cómo terminó el splash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Llegó el `quit`: se soltó el DRM y el entrante toma la pantalla.
    Handoff,
    /// Venció el temporizador de seguridad sin `quit`: se soltó el DRM para no
    /// dejar la pantalla colgada.
    TimedOut,
    /// No se pudo tomar el DRM de forma sostenida: nos rendimos y dejamos la
    /// consola, que es mejor que trabar el arranque.
    GaveUp,
}

impl Outcome {
    /// Si al terminar con este `Outcome` hay que devolver la consola a modo
    /// texto (`KD_TEXT`). Sólo el handoff deja el vt en modo gráfico: el greeter
    /// toma el scanout. El timeout y el abandono salen sin que nadie dibuje, así
    /// que restauran para no dejar la pantalla en negro ocultando el arranque.
    /// Decisión pura y sin hardware a propósito —la restauración en sí toca
    /// `/dev/tty0` y queda en `main`—, para poder fijarla con una prueba.
    pub fn restores_text_mode(self) -> bool {
        match self {
            Outcome::Handoff => false,
            Outcome::TimedOut | Outcome::GaveUp => true,
        }
    }
}

/// La función de dibujo que recibe `with_frame`: los bytes del buffer, su stride
/// y el ancho y alto en píxeles.
pub type DrawFn<'a> = dyn FnMut(&mut [u8], usize, u32, u32) + 'a;

/// Lo que el lazo necesita de la capa de DRM. El backend real lo implementa con
/// drm-rs; las pruebas, con un doble en memoria.
pub trait Backend {
    /// Toma el DRM (SET_MASTER). Puede fallar si otro proceso lo tiene.
    fn acquire(&mut self) -> std::io::Result<()>;
    /// Suelta el DRM (DROP_MASTER). No falla: en el peor caso el cierre del fd lo
    /// suelta igual.
    fn release(&mut self);
    /// Da acceso a los bytes del dumb buffer y su stride para que el lazo dibuje.
    fn with_frame(&mut self, draw: &mut DrawFn<'_>);
    /// Publica el buffer dibujado (el primer `present` es el modeset; después,
    /// pageflips).
    fn present(&mut self) -> std::io::Result<()>;
}

/// Un reloj inyectable: devuelve cuánto pasó desde que arrancó el lazo. Permite
/// probar el temporizador sin esperar en tiempo real.
pub trait Clock {
    fn elapsed(&self) -> Duration;
}

/// La fuente de órdenes: espera hasta `timeout` por la próxima orden. `None` si
/// venció la espera.
pub trait CommandSource {
    fn wait(&mut self, timeout: Duration) -> Option<Command>;
}

/// Cuántas veces reintentar SET_MASTER antes de rendirse.
const ACQUIRE_ATTEMPTS: u32 = 50;

/// Intenta tomar el DRM, reintentando hasta `ACQUIRE_ATTEMPTS` veces. Entre
/// intentos corre `backoff` (en producción, un `sleep` corto; en pruebas, un
/// no-op). Devuelve `false` si nunca lo logró.
pub fn acquire_with_retry<B: Backend>(backend: &mut B, backoff: &mut dyn FnMut()) -> bool {
    for attempt in 0..ACQUIRE_ATTEMPTS {
        if backend.acquire().is_ok() {
            return true;
        }
        if attempt + 1 < ACQUIRE_ATTEMPTS {
            backoff();
        }
    }
    false
}

/// Dibuja el fondo sólido con el logo centrado sobre el frame actual.
fn draw<B: Backend>(backend: &mut B, background: u32) {
    backend.with_frame(&mut |buf, stride, w, h| {
        render::fill(buf, stride, w, h, background);
        let (ox, oy) = geometry::center((w, h), (logo::LOGO_WIDTH, logo::LOGO_HEIGHT));
        render::blit_rgba(
            buf,
            stride,
            w,
            h,
            logo::LOGO_RGBA,
            logo::LOGO_WIDTH,
            logo::LOGO_HEIGHT,
            ox,
            oy,
        );
    });
}

/// El lazo del splash. Toma el DRM, dibuja, publica y espera el `quit` o el
/// temporizador. `background` es `0x00RRGGBB`; `timeout`, el tope de seguridad.
pub fn run<B, C, S>(
    backend: &mut B,
    clock: &C,
    source: &mut S,
    background: u32,
    timeout: Duration,
    backoff: &mut dyn FnMut(),
) -> Outcome
where
    B: Backend,
    C: Clock,
    S: CommandSource,
{
    if !acquire_with_retry(backend, backoff) {
        return Outcome::GaveUp;
    }

    draw(backend, background);
    // Si el primer `present` falla no hay nada mejor que reintentar en el
    // próximo ciclo; el buffer ya está dibujado, así que no hay parpadeo.
    let _ = backend.present();

    loop {
        match timeout.checked_sub(clock.elapsed()) {
            // Ya se pasó el tope: retirarse sí o sí.
            None => {
                backend.release();
                return Outcome::TimedOut;
            }
            Some(remaining) => match source.wait(remaining) {
                Some(Command::Quit) => {
                    backend.release();
                    return Outcome::Handoff;
                }
                Some(Command::Resume) => {
                    // Fase 2: el entrante soltó el DRM; redibujamos y seguimos.
                    // Es un stub razonable hasta que exista el hueco 2 completo.
                    let _ = backend.acquire();
                    draw(backend, background);
                    let _ = backend.present();
                }
                // Venció la espera sin orden: lo trata el `checked_sub` de arriba
                // en la próxima vuelta, que devuelve `None` y retira.
                None => {}
            },
        }
    }
}

/// `Clock` de producción: cuenta desde un `Instant` tomado al construirlo.
pub struct MonotonicClock {
    start: Instant,
}

impl Default for MonotonicClock {
    fn default() -> Self {
        MonotonicClock {
            start: Instant::now(),
        }
    }
}

impl Clock for MonotonicClock {
    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

/// `CommandSource` de producción: lee de un canal con espera acotada. El hilo del
/// socket (ver `control::serve`) es el que empuja las órdenes.
pub struct ChannelSource {
    rx: Receiver<Command>,
}

impl ChannelSource {
    pub fn new(rx: Receiver<Command>) -> Self {
        ChannelSource { rx }
    }
}

impl CommandSource for ChannelSource {
    fn wait(&mut self, timeout: Duration) -> Option<Command> {
        match self.rx.recv_timeout(timeout) {
            Ok(cmd) => Some(cmd),
            Err(RecvTimeoutError::Timeout) => None,
            // El emisor se cerró: nadie más va a mandar nada. Tratarlo como una
            // espera vencida deja que el temporizador termine de decidir.
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_el_handoff_deja_el_vt_en_modo_grafico() {
        // El handoff entrega el scanout al greeter: restaurar texto lo pisaría.
        assert!(!Outcome::Handoff.restores_text_mode());
        // Timeout y abandono salen sin que nadie dibuje: sin restaurar a texto la
        // consola queda en negro.
        assert!(Outcome::TimedOut.restores_text_mode());
        assert!(Outcome::GaveUp.restores_text_mode());
    }
}
