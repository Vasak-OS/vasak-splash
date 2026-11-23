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

use std::sync::atomic::{AtomicBool, Ordering};
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

/// Con el master **ya tomado**, dibuja y publica, pero sólo si seguimos al
/// frente; si no, suelta el master y no pinta. Es el segundo chequeo de
/// `foreground`, el que cierra la carrera del cambio de VT: entre que el lazo vio
/// la bandera en `true` y tomó el master, el hilo de VT pudo bajarla y soltar el
/// master, y sin esta segunda mirada re-tomaríamos el master y presentaríamos
/// sobre el VT del getty entrante. Si ya no estamos al frente, soltamos. (Un
/// `present` que corra justo después de un `Release` todavía más tardío falla sin
/// master y no pinta nada, así que la ventana que queda es inofensiva.)
fn present_or_yield<B: Backend>(backend: &mut B, background: u32, foreground: &AtomicBool) {
    if foreground.load(Ordering::Acquire) {
        draw(backend, background);
        // Si el `present` falla no hay nada mejor que reintentar en el próximo
        // ciclo; el buffer ya está dibujado, así que no hay parpadeo.
        let _ = backend.present();
    } else {
        backend.release();
    }
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
    foreground: &AtomicBool,
) -> Outcome
where
    B: Backend,
    C: Clock,
    S: CommandSource,
{
    if !acquire_with_retry(backend, backoff) {
        return Outcome::GaveUp;
    }

    // El primer dibujo pasa por el mismo chequeo que el redibujo: entre que se
    // instaló el hilo de VT y tomamos el master (con reintentos), un cambio de VT
    // pudo soltarlo, y presentar igual pondría el splash sobre el getty entrante.
    present_or_yield(backend, background, foreground);

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
                // Redibujar sólo si seguimos al frente. Un `Resume` lo encola el
                // hilo de VT al volver a la consola, pero puede quedar viejo: si
                // entre que se encoló y lo atendemos hubo otro cambio de VT, el
                // master ya es de otro. El guard descarta la mayoría antes de tocar
                // el DRM; `present_or_yield` vuelve a mirar la bandera DESPUÉS del
                // `acquire`, porque el hilo pudo soltar el master en el medio, y en
                // ese caso suelta en vez de robarle la pantalla al getty entrante.
                // Fase 2: el entrante soltó el DRM; redibujamos y seguimos.
                Some(Command::Resume) if foreground.load(Ordering::Acquire) => {
                    let _ = backend.acquire();
                    present_or_yield(backend, background, foreground);
                }
                // `Resume` viejo (ya no estamos al frente): descartarlo.
                Some(Command::Resume) => {}
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
    use std::collections::VecDeque;

    #[derive(Debug, PartialEq, Eq)]
    enum Event {
        Acquire,
        Release,
        Draw,
        Present,
    }

    #[derive(Default)]
    struct FakeBackend<'a> {
        events: Vec<Event>,
        acquires: usize,
        /// Simula al hilo de VT: en el acquire número N baja la bandera.
        baja_en: Option<(&'a AtomicBool, usize)>,
    }

    impl Backend for FakeBackend<'_> {
        fn acquire(&mut self) -> std::io::Result<()> {
            self.events.push(Event::Acquire);
            self.acquires += 1;
            if let Some((bandera, n)) = self.baja_en {
                if self.acquires == n {
                    bandera.store(false, Ordering::Release);
                }
            }
            Ok(())
        }
        fn release(&mut self) {
            self.events.push(Event::Release);
        }
        fn with_frame(&mut self, _draw: &mut DrawFn<'_>) {
            self.events.push(Event::Draw);
        }
        fn present(&mut self) -> std::io::Result<()> {
            self.events.push(Event::Present);
            Ok(())
        }
    }

    struct ZeroClock;
    impl Clock for ZeroClock {
        fn elapsed(&self) -> Duration {
            Duration::ZERO
        }
    }

    struct Script<'a> {
        ordenes: VecDeque<Command>,
        /// Simula al hilo de VT: baja la bandera antes de entregar la primera orden
        /// (un cambio de VT ocurrió después de encolar el `Resume`).
        baja_antes: Option<&'a AtomicBool>,
    }
    impl CommandSource for Script<'_> {
        fn wait(&mut self, _timeout: Duration) -> Option<Command> {
            if let Some(bandera) = self.baja_antes.take() {
                bandera.store(false, Ordering::Release);
            }
            // El guion siempre termina en `Quit`, así que nunca se agota.
            self.ordenes.pop_front()
        }
    }

    /// Corre el lazo con el guion dado y devuelve cómo terminó y qué hizo el backend.
    fn correr(foreground: bool, baja_antes: bool, guion: Vec<Command>) -> (Outcome, Vec<Event>) {
        let bandera = AtomicBool::new(foreground);
        let mut backend = FakeBackend::default();
        let mut source = Script {
            ordenes: guion.into(),
            baja_antes: baja_antes.then_some(&bandera),
        };
        let outcome = run(
            &mut backend,
            &ZeroClock,
            &mut source,
            0,
            Duration::from_secs(60),
            &mut || {},
            &bandera,
        );
        (outcome, backend.events)
    }

    #[test]
    fn un_resume_viejo_no_redibuja_ni_retoma_el_master_si_ya_no_estamos_al_frente() {
        let (outcome, events) = correr(true, true, vec![Command::Resume, Command::Quit]);
        assert_eq!(outcome, Outcome::Handoff);
        // Sólo el arranque: acquire + draw + present, y el release del handoff.
        assert_eq!(
            events,
            vec![Event::Acquire, Event::Draw, Event::Present, Event::Release]
        );
    }

    #[test]
    fn un_resume_al_frente_retoma_el_master_y_redibuja() {
        let (outcome, events) = correr(true, false, vec![Command::Resume, Command::Quit]);
        assert_eq!(outcome, Outcome::Handoff);
        assert_eq!(
            events,
            vec![
                Event::Acquire,
                Event::Draw,
                Event::Present,
                Event::Acquire,
                Event::Draw,
                Event::Present,
                Event::Release,
            ]
        );
    }

    #[test]
    fn si_el_master_se_suelta_durante_el_acquire_del_resume_no_se_dibuja_y_se_cede() {
        // Al frente cuando el lazo evalúa el guard, pero el hilo de VT baja la
        // bandera justo mientras se toma el master (el 2.º acquire).
        let foreground = AtomicBool::new(true);
        let mut backend = FakeBackend {
            baja_en: Some((&foreground, 2)),
            ..Default::default()
        };
        let mut source = Script {
            ordenes: vec![Command::Resume, Command::Quit].into(),
            baja_antes: None,
        };
        let outcome = run(
            &mut backend,
            &ZeroClock,
            &mut source,
            0,
            Duration::from_secs(60),
            &mut || {},
            &foreground,
        );
        assert_eq!(outcome, Outcome::Handoff);
        assert_eq!(
            backend.events,
            vec![
                Event::Acquire,
                Event::Draw,
                Event::Present,
                // El Resume: toma, ve la bandera baja y suelta, sin dibujar.
                Event::Acquire,
                Event::Release,
                Event::Release,
            ]
        );
    }

    #[test]
    fn si_ya_no_estamos_al_frente_al_arrancar_se_cede_el_master_sin_dibujar() {
        let (outcome, events) = correr(false, false, vec![Command::Quit]);
        assert_eq!(outcome, Outcome::Handoff);
        assert_eq!(events, vec![Event::Acquire, Event::Release, Event::Release]);
    }

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
