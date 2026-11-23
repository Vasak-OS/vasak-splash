//! `vasak-splashd`: el splash de arranque de VasakOS.
//!
//! Sin argumentos (o con `start`) **es** el splash: toma el DRM, dibuja el logo
//! sobre un fondo sólido y espera el `quit`. Con `quit` o `resume` es un cliente
//! que le manda esa orden al daemon por el socket de control.
//!
//! Fase 1: sólo el hueco boot→greeter. El daemon arranca en el initramfs como
//! dueño del DRM y se retira al primer frame del greeter, que es quien manda el
//! `quit`. El hueco greeter→compositor (Fase 2) deja `resume` como stub.

mod config;
mod control;
mod drm_backend;
mod geometry;
mod logo;
mod render;
mod run;
mod select;
mod vt;

use std::os::unix::io::AsRawFd;
use std::process::ExitCode;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use run::{MasterGate, Outcome};

/// Dónde busca el daemon su configuración. Si no está, valores por omisión.
const CONFIG_PATH: &str = "/etc/vasak-splash.conf";

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        None | Some("start") | Some("run") => start(),
        Some("quit") => client("quit"),
        Some("resume") => client("resume"),
        Some("--version" | "version") => {
            println!("vasak-splashd {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h" | "help") => {
            print_usage();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("vasak-splashd: subcomando desconocido: {other}");
            print_usage();
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "uso: vasak-splashd [start|quit|resume|version|help]\n\
         \n\
         start    (por omisión) toma el DRM y muestra el splash\n\
         quit     le dice al daemon que se retire (handoff al greeter)\n\
         resume   Fase 2 (stub): que el daemon vuelva a tomar el DRM\n\
         version  imprime la versión\n\
         help     esta ayuda"
    );
}

/// El daemon: arma el backend real, levanta el socket y corre el lazo.
fn start() -> ExitCode {
    let config = std::fs::read_to_string(CONFIG_PATH)
        .map(|text| config::parse(&text))
        .unwrap_or_default();

    // Bloquear las señales de cambio de VT antes de crear ningún hilo (el del
    // socket incluido): tienen que llegar sólo por el signalfd del hilo de VT, no
    // matar el proceso.
    vt::block_switch_signals();

    let mut backend = match drm_backend::DrmBackend::open() {
        Ok(backend) => backend,
        Err(err) => {
            // No se pudo tomar la pantalla: rendirse y dejar la consola es mejor
            // que trabar el arranque.
            eprintln!("vasak-splashd: no se pudo preparar el DRM: {err}");
            return ExitCode::FAILURE;
        }
    };

    // La consola del splash, fijada a su propio VT (no al flotante `/dev/tty0`):
    // con ella se restaura el modo texto y el `VT_SETMODE` aunque el usuario haya
    // cambiado de consola. Se abre antes del socket para que el camino de fallo de
    // abajo pueda devolverla a modo texto.
    let tty = vt::open_console();

    let (tx, rx) = mpsc::channel();
    if let Err(err) = control::serve(control::SOCKET_PATH, tx.clone()) {
        eprintln!("vasak-splashd: no se pudo abrir el socket de control: {err}");
        // `open()` ya dejó el vt en modo gráfico: sin nadie que dibuje, hay que
        // devolver la consola a modo texto.
        if let Some(tty) = &tty {
            drm_backend::set_text_mode(tty.as_raw_fd());
        }
        return ExitCode::FAILURE;
    }

    // Cooperar con el cambio de consola: sin esto el splash se queda con el master
    // del DRM y el vt en modo gráfico, así que mientras está arriba ningún TTY se
    // dibuja —te quedás sin consola—. El `MasterGate` lo comparten el hilo de VT y
    // el lazo: lleva la bandera de «al frente» (para descartar los `Resume` viejos)
    // y serializa el master con el `present`. Best-effort: si no hay consola real
    // (p. ej. en desarrollo), el daemon sigue igual, sin cooperación.
    let gate = Arc::new(MasterGate::new(true));
    if let Some(tty) = &tty {
        match backend.clone_card_fd() {
            Ok(card_fd) => {
                if let Err(err) = vt::install(tty.as_raw_fd(), card_fd, tx, Arc::clone(&gate)) {
                    eprintln!("vasak-splashd: sin cooperación de cambio de VT: {err}");
                }
            }
            Err(err) => eprintln!("vasak-splashd: no se pudo duplicar el fd de la GPU: {err}"),
        }
    }

    let clock = run::MonotonicClock::default();
    let mut source = run::ChannelSource::new(rx);
    // Entre reintentos de SET_MASTER, una pausa corta. 50 × 100 ms ≈ 5 s de
    // insistencia antes de rendirse, bastante para que el proceso que tenga el
    // master lo suelte.
    let mut backoff = || std::thread::sleep(Duration::from_millis(100));

    let outcome = run::run(
        &mut backend,
        &clock,
        &mut source,
        config.background,
        config.timeout,
        &mut backoff,
        &gate,
    );

    // Devolver el VT a modo automático antes de salir: el greeter y los gettys
    // tienen que poder cambiar de consola sin un daemon que ya no está.
    if let Some(tty) = &tty {
        vt::restore(tty.as_raw_fd());
    }

    // La decisión «¿restaurar el vt?» es pura y vive en `Outcome` para poder
    // fijarla con una prueba; la restauración en sí toca el VT y queda acá.
    if outcome.restores_text_mode() {
        if let Some(tty) = &tty {
            drm_backend::set_text_mode(tty.as_raw_fd());
        }
    }

    match outcome {
        Outcome::Handoff | Outcome::TimedOut => ExitCode::SUCCESS,
        Outcome::GaveUp => ExitCode::FAILURE,
    }
}

/// El cliente: manda una orden al daemon por el socket.
fn client(command: &str) -> ExitCode {
    match control::send(control::SOCKET_PATH, command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("vasak-splashd: no se pudo contactar al daemon: {err}");
            ExitCode::FAILURE
        }
    }
}
