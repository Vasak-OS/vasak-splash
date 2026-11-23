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
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::control::Command;
use crate::{geometry, logo, render};

/// Serializa el master del DRM entre el lazo —que presenta— y el hilo de VT —que
/// lo suelta en un cambio de consola y lo retoma al volver—.
///
/// La carrera que cierra: el `present` del lazo es un `SETCRTC`, cuya autorización
/// (el chequeo de master) la hace el kernel *antes* de ejecutar el ioctl. Sin
/// serializar, el lazo puede ver la bandera en `true`, y entre eso y el `present`
/// el hilo de VT baja la bandera y suelta el master; el scanout del splash podría
/// entonces caer sobre el VT entrante. El lock lo vuelve imposible: el lazo
/// presenta con el lock tomado, y el hilo suelta/retoma el master con el mismo
/// lock, así que un `present` corre entero bajo el master o queda excluido hasta
/// después de retomarlo. La bandera dice si seguimos al frente; el lock garantiza
/// que el «chequear y presentar» sea atómico respecto del «bajar la bandera y
/// soltar el master».
pub struct MasterGate {
    foreground: AtomicBool,
    lock: Mutex<()>,
}

impl MasterGate {
    pub fn new(foreground: bool) -> Self {
        MasterGate {
            foreground: AtomicBool::new(foreground),
            lock: Mutex::new(()),
        }
    }

    /// Chequeo barato de si seguimos al frente, sin tomar el lock, para descartar
    /// un `Resume` viejo. El chequeo que cierra la carrera es el de `present`.
    pub fn is_foreground(&self) -> bool {
        self.foreground.load(Ordering::Acquire)
    }

    /// Con el master **ya tomado**, corre `present` sólo si seguimos al frente, con
    /// el lock tomado para que el hilo de VT no pueda soltar el master en el medio.
    /// Si un envenenamiento dejó el lock tomado por un pánico, se recupera el guard:
    /// trabar el splash dejaría la pantalla colgada, que es peor.
    fn present_guarded(&self, present: impl FnOnce()) {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if self.foreground.load(Ordering::Acquire) {
            present();
        }
    }

    /// El hilo de VT en `Release`: baja la bandera y suelta el master en una sola
    /// sección crítica, excluyente con `present_guarded`.
    pub fn release_master(&self, drop_master: impl FnOnce()) {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.foreground.store(false, Ordering::Release);
        drop_master();
    }

    /// El hilo de VT en `Acquire`: retoma el master y sube la bandera en una sola
    /// sección crítica, excluyente con `present_guarded`.
    pub fn acquire_master(&self, set_master: impl FnOnce()) {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        set_master();
        self.foreground.store(true, Ordering::Release);
    }
}

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

/// Con el master **ya tomado** (por el `acquire` del arranque o por el
/// `set_master` del hilo de VT al volver a la consola), dibuja y publica, pero
/// sólo si seguimos al frente, todo bajo el lock de `MasterGate` para que el hilo
/// de VT no pueda soltar el master entre el chequeo y el `present`. Pintar sin
/// master pondría el splash sobre el VT del getty entrante; el lock lo impide.
///
/// Si ya no estamos al frente, no hace nada: **no suelta el master**. El dueño de
/// soltarlo en un cambio de VT es el hilo (su `drop_master` en `Release`), y el fd
/// del hilo es un dup del del lazo —comparten el mismo master—, así que cuando la
/// bandera está en `false` el master ya lo soltó el hilo. Si el lazo lo soltara
/// acá, podría pisar un `set_master` que el hilo acabara de hacer al volver (el
/// master es compartido), y el redibujo siguiente se quedaría sin master. El lazo
/// sólo suelta el master en su salida final (`Handoff`/`TimedOut`).
fn present_if_foreground<B: Backend>(backend: &mut B, background: u32, gate: &MasterGate) {
    gate.present_guarded(|| {
        draw(backend, background);
        // Si el `present` falla no hay nada mejor que reintentar en el próximo
        // ciclo; el buffer ya está dibujado, así que no hay parpadeo.
        let _ = backend.present();
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
    gate: &MasterGate,
) -> Outcome
where
    B: Backend,
    C: Clock,
    S: CommandSource,
{
    if !acquire_with_retry(backend, backoff) {
        return Outcome::GaveUp;
    }

    // El primer dibujo pasa por el mismo chequeo que el redibujo. En el arranque
    // (initramfs) todavía no hay otra consola a la que cambiar, así que la bandera
    // está en `true`; el chequeo cubre el caso raro de un cambio de VT entre que se
    // instaló el hilo y tomamos el master.
    present_if_foreground(backend, background, gate);

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
                // Redibujar al volver a la consola, sin re-tomar el master: ya lo
                // re-tomó el hilo de VT en el `Acquire` (su `set_master`) antes de
                // encolar este `Resume`, así que el lazo sólo dibuja. Es la clave de
                // que no se le robe la pantalla al getty: el lazo nunca hace
                // `acquire` acá, y un `Resume` viejo —otro cambio de VT soltó el
                // master en el medio— lo descarta el guard, o si se coló, su
                // `present` sin master falla solo. El guard es el chequeo barato;
                // `present_if_foreground` vuelve a mirar la bandera antes de pintar.
                // shortcut: la Fase 2 (hueco greeter→compositor) necesitará re-tomar
                // el master acá, porque ahí lo soltó el compositor y no el hilo de
                // VT; se agrega cuando ese hueco se cablee de verdad.
                Some(Command::Resume) if gate.is_foreground() => {
                    present_if_foreground(backend, background, gate);
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
    struct FakeBackend {
        events: Vec<Event>,
    }

    impl Backend for FakeBackend {
        fn acquire(&mut self) -> std::io::Result<()> {
            self.events.push(Event::Acquire);
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
        /// Simula al hilo de VT: baja la bandera (soltando el master) antes de
        /// entregar la primera orden —un cambio de VT ocurrió después de encolar el
        /// `Resume`—.
        baja_antes: Option<&'a MasterGate>,
    }
    impl CommandSource for Script<'_> {
        fn wait(&mut self, _timeout: Duration) -> Option<Command> {
            if let Some(gate) = self.baja_antes.take() {
                gate.release_master(|| {});
            }
            // El guion siempre termina en `Quit`, así que nunca se agota.
            self.ordenes.pop_front()
        }
    }

    /// Corre el lazo con el guion dado y devuelve cómo terminó y qué hizo el backend.
    fn correr(foreground: bool, baja_antes: bool, guion: Vec<Command>) -> (Outcome, Vec<Event>) {
        let gate = MasterGate::new(foreground);
        let mut backend = FakeBackend::default();
        let mut source = Script {
            ordenes: guion.into(),
            baja_antes: baja_antes.then_some(&gate),
        };
        let outcome = run(
            &mut backend,
            &ZeroClock,
            &mut source,
            0,
            Duration::from_secs(60),
            &mut || {},
            &gate,
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
    fn un_resume_al_frente_redibuja_sin_retomar_el_master() {
        let (outcome, events) = correr(true, false, vec![Command::Resume, Command::Quit]);
        assert_eq!(outcome, Outcome::Handoff);
        assert_eq!(
            events,
            vec![
                Event::Acquire,
                Event::Draw,
                Event::Present,
                Event::Draw,
                Event::Present,
                Event::Release,
            ]
        );
    }

    #[test]
    fn el_resume_nunca_toma_el_master_el_unico_acquire_es_el_del_arranque() {
        // El master lo re-toma el hilo de VT antes de encolar el `Resume`: si el
        // lazo lo tomara otra vez podría robarle la pantalla al VT entrante.
        let (_, events) = correr(true, false, vec![Command::Resume, Command::Quit]);
        let acquires = events.iter().filter(|e| **e == Event::Acquire).count();
        assert_eq!(acquires, 1, "eventos: {events:?}");
    }

    #[test]
    fn si_no_estamos_al_frente_al_arrancar_no_dibuja_ni_suelta_el_master() {
        // Soltar en un cambio de VT es cosa del hilo de VT: el lazo no lo pisa.
        // Sólo queda el `Release` del `Quit`.
        let (outcome, events) = correr(false, false, vec![Command::Quit]);
        assert_eq!(outcome, Outcome::Handoff);
        assert_eq!(events, vec![Event::Acquire, Event::Release]);
    }

    #[test]
    fn el_lazo_solo_suelta_el_master_en_la_salida_final() {
        // Un `Resume` al frente redibuja pero no suelta: el único `Release` es el
        // del `Quit`, y es el último evento.
        let (_, events) = correr(true, false, vec![Command::Resume, Command::Quit]);
        let releases = events.iter().filter(|e| **e == Event::Release).count();
        assert_eq!(releases, 1, "eventos: {events:?}");
        assert_eq!(events.last(), Some(&Event::Release));
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

#[cfg(test)]
mod master_gate_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn no_presenta_si_la_bandera_esta_en_false() {
        let gate = MasterGate::new(false);
        let mut corrio = false;
        gate.present_guarded(|| corrio = true);
        assert!(!corrio);
        assert!(!gate.is_foreground());
    }

    #[test]
    fn presenta_si_la_bandera_esta_en_true() {
        let gate = MasterGate::new(true);
        let mut corrio = false;
        gate.present_guarded(|| corrio = true);
        assert!(corrio);
    }

    #[test]
    fn soltar_baja_la_bandera_y_retomar_la_sube_y_permite_presentar() {
        let gate = MasterGate::new(true);
        let mut soltado = false;
        gate.release_master(|| soltado = true);
        assert!(soltado);
        assert!(!gate.is_foreground());

        let mut presentado = false;
        gate.present_guarded(|| presentado = true);
        assert!(!presentado, "sin master no se presenta");

        let mut retomado = false;
        gate.acquire_master(|| retomado = true);
        assert!(retomado);
        assert!(gate.is_foreground());

        gate.present_guarded(|| presentado = true);
        assert!(presentado, "tras retomar se vuelve a presentar");
    }

    /// Un `present` nunca debe ver el master suelto, aunque el hilo de VT lo
    /// suelte y lo retome a la vez. Se ensancha la ventana con `yield_now` dentro
    /// del `present` (y del `drop_master`) para que, sin el lock, la violación
    /// aparezca en cada corrida y no sólo de vez en cuando.
    #[test]
    fn un_present_nunca_ve_el_master_suelto_con_soltar_y_retomar_concurrentes() {
        const ITERACIONES: usize = 2000;
        let gate = Arc::new(MasterGate::new(true));
        let master = Arc::new(AtomicBool::new(true));
        let violaciones = Arc::new(AtomicUsize::new(0));
        let presentados = Arc::new(AtomicUsize::new(0));
        let barrera = Arc::new(Barrier::new(2));

        let vt = {
            let (gate, master, barrera) = (gate.clone(), master.clone(), barrera.clone());
            thread::spawn(move || {
                for _ in 0..ITERACIONES {
                    barrera.wait();
                    gate.release_master(|| {
                        thread::yield_now();
                        master.store(false, Ordering::SeqCst);
                    });
                    thread::yield_now();
                    gate.acquire_master(|| master.store(true, Ordering::SeqCst));
                }
            })
        };

        for _ in 0..ITERACIONES {
            barrera.wait();
            for _ in 0..4 {
                gate.present_guarded(|| {
                    presentados.fetch_add(1, Ordering::SeqCst);
                    let mut visto_suelto = !master.load(Ordering::SeqCst);
                    thread::yield_now();
                    visto_suelto |= !master.load(Ordering::SeqCst);
                    if visto_suelto {
                        violaciones.fetch_add(1, Ordering::SeqCst);
                    }
                });
            }
        }
        vt.join().unwrap();

        assert_eq!(violaciones.load(Ordering::SeqCst), 0);
        assert!(presentados.load(Ordering::SeqCst) > 0);
        assert!(gate.is_foreground() && master.load(Ordering::SeqCst));
    }
}
