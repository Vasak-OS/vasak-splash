//! Cooperación con el cambio de consola virtual (VT).
//!
//! El splash toma el master del DRM y deja el vt en modo gráfico. Si no maneja
//! el cambio de VT, mientras el splash está arriba **ningún TTY se dibuja**:
//! quien aprieta Ctrl+Alt+F2 se queda sin consola —el master sigue nuestro y la
//! pantalla no cambia—. Igual que plymouth, acá ponemos el vt en `VT_PROCESS`
//! para que el kernel nos avise por señal de cada cambio, y en cada uno soltamos
//! el master antes de ceder la pantalla y lo retomamos al volver (redibujando
//! por el canal de control, que reusa `Resume`).
//!
//! Las señales se leen por un `signalfd` desde un hilo propio, no por un
//! manejador asíncrono: así el trabajo (ioctls, mandar `Resume`) corre fuera del
//! contexto de señal, sin sus restricciones.
//!
//! `libc` 0.2 no trae las constantes de `<linux/vt.h>` ni `vt_mode`, así que van
//! acá como literales —igual que el `KDSETMODE` de `drm_backend`—. El camino es
//! de ioctls crudos sobre una consola real: no se puede probar sin hardware, así
//! que sólo la decisión señal→acción (`action_for_signal`) queda detrás de una
//! función pura.

use std::fs::OpenOptions;
use std::io;
use std::os::unix::io::{AsRawFd, OwnedFd, RawFd};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;

use crate::control::Command;
use crate::run::MasterGate;

// <linux/vt.h>
const VT_GETSTATE: libc::c_ulong = 0x5603;
const VT_SETMODE: libc::c_ulong = 0x5602;
const VT_RELDISP: libc::c_ulong = 0x5605;
const VT_AUTO: libc::c_char = 0x00;
const VT_PROCESS: libc::c_char = 0x01;
/// Reconocer la toma de la consola (arg de `VT_RELDISP`).
const VT_ACKACQ: libc::c_long = 0x02;
/// Permitir que el cambio de consola siga adelante (arg de `VT_RELDISP`).
const VT_RELDISP_ALLOW: libc::c_long = 1;

// <drm/drm.h>: DRM_IOCTL_{SET,DROP}_MASTER = _IO('d', 0x1e | 0x1f).
const DRM_IOCTL_SET_MASTER: libc::c_ulong = 0x641e;
const DRM_IOCTL_DROP_MASTER: libc::c_ulong = 0x641f;

#[repr(C)]
struct VtMode {
    mode: libc::c_char,
    waitv: libc::c_char,
    relsig: libc::c_short,
    acqsig: libc::c_short,
    frsig: libc::c_short,
}

#[repr(C)]
struct VtStat {
    v_active: libc::c_ushort,
    v_signal: libc::c_ushort,
    v_state: libc::c_ushort,
}

/// Abre la consola del splash **fijada a su VT**. `/dev/tty0` es flotante —apunta
/// siempre al VT activo del momento—, así que poner o quitar el modo gráfico o el
/// `VT_SETMODE` a través de él caen sobre el VT que esté al frente, no sobre el
/// nuestro: si el usuario cambió de consola, restauraríamos el VT equivocado y el
/// nuestro quedaría en modo gráfico o en `VT_PROCESS` colgado —pantalla negra al
/// volver—. Por eso resolvemos el VT activo al arrancar, cuando el nuestro sí
/// está al frente, con `VT_GETSTATE`, y abrimos `/dev/ttyN`. Si algo falla, se cae
/// al `/dev/tty0` ya abierto —best-effort, como el resto del camino de DRM—.
pub fn open_console() -> Option<OwnedFd> {
    let tty0 = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty0")
        .ok()?;
    let mut st: VtStat = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(tty0.as_raw_fd(), VT_GETSTATE, &mut st) } < 0 {
        return Some(OwnedFd::from(tty0));
    }
    match OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!("/dev/tty{}", st.v_active))
    {
        Ok(f) => Some(OwnedFd::from(f)),
        Err(_) => Some(OwnedFd::from(tty0)),
    }
}

/// Qué hacer ante una señal del kernel sobre el VT. Pura y sin hardware a
/// propósito, para poder fijarla con una prueba; el resto del módulo son ioctls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VtAction {
    /// El kernel pide ceder la consola (`relsig`): soltar el master y permitir
    /// el cambio.
    Release,
    /// Volvimos a la consola (`acqsig`): retomar el master y redibujar.
    Acquire,
    /// Cualquier otra señal que llegue por el mismo fd: ignorar.
    Ignore,
}

/// La señal que manda el kernel se traduce a una acción. `SIGUSR1`/`SIGUSR2` son
/// los que pedimos en `VT_SETMODE`; cualquier otro es ruido.
pub fn action_for_signal(signo: i32) -> VtAction {
    match signo {
        libc::SIGUSR1 => VtAction::Release,
        libc::SIGUSR2 => VtAction::Acquire,
        _ => VtAction::Ignore,
    }
}

/// Si al recibir un `acqsig` hay que reclamar el master: sólo cuando nuestro VT
/// (`our_vt`, resuelto en el arranque) sigue siendo el activo (`active`, de
/// `VT_GETSTATE`). `signalfd` puede entregar un `SIGUSR2` que quedó viejo —incluso
/// fuera del orden en que el kernel lo generó, tras un `SIGUSR1`—: reclamar a
/// ciegas tomaría el master con otro VT al frente y le robaría la pantalla al
/// getty. Si no se pudo leer alguno de los dos VT, **no** reclama: sin confirmar
/// que nuestro VT está al frente, tomar el master es el mismo riesgo de robarle la
/// pantalla al getty, y un redibujo perdido pesa menos que eso. Pura a propósito,
/// para fijarla con una prueba; el `VT_GETSTATE` en sí queda en `vt_active`.
pub fn should_reclaim(our_vt: Option<u16>, active: Option<u16>) -> bool {
    matches!((our_vt, active), (Some(nuestro), Some(activo)) if nuestro == activo)
}

/// El conjunto de señales de cambio de VT (`SIGUSR1`, `SIGUSR2`).
fn switch_sigset() -> libc::sigset_t {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        libc::sigaddset(&mut set, libc::SIGUSR2);
        set
    }
}

/// Bloquea `SIGUSR1`/`SIGUSR2` en el proceso. Hay que llamarla **antes** de
/// crear cualquier hilo —el del socket incluido—: los hilos heredan la máscara,
/// y así las señales llegan sólo por el `signalfd` en vez de matar el proceso
/// (su acción por omisión).
pub fn block_switch_signals() {
    let set = switch_sigset();
    unsafe {
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

/// Pone el VT en `VT_PROCESS` (el kernel nos señala los cambios) y lanza el hilo
/// que los atiende. `tty` es el fd de nuestra consola (el de `open_console`, fijo
/// a nuestro VT); `card`, un dup del fd de la GPU para soltar/retomar el master;
/// `tx`, el canal del lazo, por donde se pide el redibujo al volver; `gate`, el
/// `MasterGate` compartido con el lazo: por él se baja/sube la bandera —para que
/// descarte los `Resume` viejos— y se suelta/retoma el master bajo el lock, de modo
/// que un `present` del lazo no pueda colarse mientras el master está suelto. Si el
/// `VT_SETMODE` falla (sin consola real, p. ej. en desarrollo), devuelve el error y
/// el daemon sigue sin cooperación de VT —como el resto del camino de DRM, es
/// best-effort—.
pub fn install(
    tty: RawFd,
    card: OwnedFd,
    tx: Sender<Command>,
    gate: Arc<MasterGate>,
) -> io::Result<()> {
    set_process_mode(tty)?;

    let set = switch_sigset();
    let sfd = unsafe { libc::signalfd(-1, &set, libc::SFD_CLOEXEC) };
    if sfd < 0 {
        // No se pudo abrir el signalfd: devolver el VT a modo automático para no
        // dejarlo en VT_PROCESS sin nadie que conteste los cambios (quedaría
        // colgado el switch).
        restore(tty);
        return Err(io::Error::last_os_error());
    }

    thread::spawn(move || {
        // El dup del fd de la GPU vive lo que vive el hilo: por él se suelta y se
        // retoma el master, que es por descripción de archivo (el dup comparte
        // la del lazo, así que afecta al mismo master).
        let card_fd = card.as_raw_fd();
        // Nuestro VT, resuelto mientras el splash está al frente (en el arranque),
        // para después descartar un `SIGUSR2` viejo que llegaría con otro VT activo.
        let our_vt = vt_active(tty);
        let size = std::mem::size_of::<libc::signalfd_siginfo>();
        let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
        loop {
            let n =
                unsafe { libc::read(sfd, (&mut info as *mut libc::signalfd_siginfo).cast(), size) };
            if n as usize != size {
                // EINTR: reintentar. Cualquier otro error en el signalfd es
                // definitivo —sin él no hay más cooperación—: salir del hilo.
                if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            match action_for_signal(info.ssi_signo as i32) {
                VtAction::Release => {
                    // Bajar la bandera y soltar el master en una sola sección crítica
                    // (bajo el lock de `gate`), excluyente con el `present` del lazo:
                    // así un `present` no se cuela entre que bajamos la bandera y
                    // soltamos el master —la ventana en que el scanout del splash
                    // podía caer sobre el VT entrante—. Bajar la bandera primero hace,
                    // además, que un `Resume` viejo que el lazo no atendió se descarte
                    // en vez de robarle la pantalla al VT entrante. Recién después el
                    // reldisp, para que el kernel dé la pantalla al getty entrante.
                    gate.release_master(|| drop_master(card_fd));
                    reldisp(tty, VT_RELDISP_ALLOW);
                }
                VtAction::Acquire => {
                    reldisp(tty, VT_ACKACQ);
                    // Reclamar el master sólo si (1) nuestro VT sigue siendo el activo
                    // —un `SIGUSR2` viejo, que signalfd puede entregar fuera del orden
                    // en que el kernel lo generó tras un `SIGUSR1`, llegaría con otro VT
                    // al frente— y (2) `SET_MASTER` de verdad funcionó. Sólo entonces
                    // pedir el redibujo: `acquire_master` ya subió la bandera, bajo el
                    // lock, nada más porque lo tomamos. Pedir `Resume` sin master haría
                    // que el lazo presente sobre un VT ajeno.
                    if should_reclaim(our_vt, vt_active(tty))
                        && gate.acquire_master(|| set_master(card_fd))
                    {
                        // El lazo redibuja al recibir Resume (reusa el hueco Fase 2).
                        // Si el lazo ya terminó, el canal está cerrado: nada que hacer.
                        if tx.send(Command::Resume).is_err() {
                            break;
                        }
                    }
                }
                VtAction::Ignore => {}
            }
        }
    });
    Ok(())
}

/// Devuelve el VT a `VT_AUTO` (el kernel vuelve a manejar los cambios solo). Se
/// llama al salir, para que el greeter y los gettys cambien de consola sin
/// depender de un daemon que ya no está.
pub fn restore(tty: RawFd) {
    let mode = VtMode {
        mode: VT_AUTO,
        waitv: 0,
        relsig: 0,
        acqsig: 0,
        frsig: 0,
    };
    unsafe {
        libc::ioctl(tty, VT_SETMODE, &mode as *const VtMode);
    }
}

fn set_process_mode(tty: RawFd) -> io::Result<()> {
    let mode = VtMode {
        mode: VT_PROCESS,
        waitv: 0,
        relsig: libc::SIGUSR1 as libc::c_short,
        acqsig: libc::SIGUSR2 as libc::c_short,
        frsig: 0,
    };
    let r = unsafe { libc::ioctl(tty, VT_SETMODE, &mode as *const VtMode) };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn reldisp(tty: RawFd, arg: libc::c_long) {
    unsafe {
        libc::ioctl(tty, VT_RELDISP, arg);
    }
}

fn drop_master(fd: RawFd) {
    unsafe {
        libc::ioctl(fd, DRM_IOCTL_DROP_MASTER, 0);
    }
}

/// Intenta tomar el master (`SET_MASTER`). Devuelve si lo logró: puede fallar con
/// `EBUSY` si otro cliente lo tiene, y en ese caso el llamador no debe darse por
/// al frente ni redibujar.
fn set_master(fd: RawFd) -> bool {
    unsafe { libc::ioctl(fd, DRM_IOCTL_SET_MASTER, 0) >= 0 }
}

/// El VT activo ahora mismo (`VT_GETSTATE`), o `None` si el ioctl falla.
fn vt_active(tty: RawFd) -> Option<u16> {
    let mut st: VtStat = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(tty, VT_GETSTATE, &mut st) } < 0 {
        return None;
    }
    Some(st.v_active)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigusr1_es_ceder_la_consola() {
        assert_eq!(action_for_signal(libc::SIGUSR1), VtAction::Release);
    }

    #[test]
    fn sigusr2_es_retomar_la_consola() {
        assert_eq!(action_for_signal(libc::SIGUSR2), VtAction::Acquire);
    }

    #[test]
    fn cualquier_otra_senal_se_ignora() {
        for signo in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, 0, -1] {
            assert_eq!(action_for_signal(signo), VtAction::Ignore, "signo {signo}");
        }
    }

    #[test]
    fn con_nuestro_vt_al_frente_se_reclama_el_master() {
        assert!(should_reclaim(Some(1), Some(1)));
    }

    #[test]
    fn con_otro_vt_al_frente_no_se_reclama_el_master() {
        // Un SIGUSR2 viejo llega con otro VT activo: reclamar le robaría la pantalla.
        assert!(!should_reclaim(Some(1), Some(2)));
    }

    #[test]
    fn sin_saber_nuestro_vt_no_se_reclama_el_master() {
        assert!(!should_reclaim(None, Some(2)));
    }

    #[test]
    fn sin_saber_el_vt_activo_no_se_reclama_el_master() {
        // Sin confirmar el VT activo no se le roba la pantalla al getty.
        assert!(!should_reclaim(Some(1), None));
    }

    #[test]
    fn sin_saber_ninguno_de_los_dos_vt_no_se_reclama_el_master() {
        assert!(!should_reclaim(None, None));
    }
}
