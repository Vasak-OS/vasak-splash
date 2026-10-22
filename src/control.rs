//! El protocolo de control por socket unix. El daemon escucha en
//! `/run/vasak-splash.sock`; un `vasak-splashd quit` se conecta y escribe una
//! línea. El parseo está separado del socket para poder probarlo sin red.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc::Sender;
use std::thread;

/// Ruta del socket de control. 0660 root:vasak-splash: lo crea el daemon (root
/// en el arranque) y el greeter lo usa para mandar `quit` —de ahí el grupo—.
pub const SOCKET_PATH: &str = "/run/vasak-splash.sock";

/// Las órdenes que entiende el daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Retirarse: el entrante ya dibujó, soltar el DRM y salir dejando el último
    /// frame. Es el `quit` del hueco boot→greeter.
    Quit,
    /// Volver a tomar el DRM y redibujar. Hueco greeter→compositor (Fase 2); por
    /// ahora el daemon lo trata como un redibujo best-effort.
    Resume,
}

/// Interpreta una línea del socket. Ignora espacios y mayúsculas; `None` para
/// cualquier cosa que no reconozca.
pub fn parse_command(line: &str) -> Option<Command> {
    match line.trim().to_ascii_lowercase().as_str() {
        "quit" => Some(Command::Quit),
        "resume" => Some(Command::Resume),
        _ => None,
    }
}

/// Levanta el listener, le pone los permisos y lanza un hilo que acepta
/// conexiones y empuja cada orden reconocida por `tx`. El hilo vive lo que viva
/// el daemon; cuando el daemon sale, el proceso se lleva el socket.
pub fn serve(path: &str, tx: Sender<Command>) -> std::io::Result<()> {
    // Un socket viejo de un arranque anterior impediría el bind.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    set_socket_perms(path);

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            if let Some(cmd) = read_command(stream) {
                // Si el receptor ya se fue, el daemon está saliendo: nada que hacer.
                if tx.send(cmd).is_err() {
                    break;
                }
            }
        }
    });
    Ok(())
}

/// Lee la primera línea de una conexión y la parsea.
fn read_command(stream: UnixStream) -> Option<Command> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    parse_command(&line)
}

/// 0660 y grupo `vasak-splash` si el grupo existe. Los errores se ignoran a
/// propósito: en el arranque el daemon es root y puede hablarse a sí mismo igual;
/// el grupo sólo hace falta cuando el greeter (otro usuario) mande el `quit`, y
/// eso es trabajo de integración aparte.
fn set_socket_perms(path: &str) {
    if let Ok(perms) = std::fs::metadata(path).map(|m| {
        let mut p = m.permissions();
        p.set_mode(0o660);
        p
    }) {
        let _ = std::fs::set_permissions(path, perms);
    }
    if let Some(gid) = group_gid("vasak-splash") {
        // chown(path, uid=-1 => sin cambio, gid).
        let _ = std::os::unix::fs::chown(path, None, Some(gid));
    }
}

/// Resuelve el gid de un grupo leyendo `/etc/group`, sin arrastrar una
/// dependencia por tres líneas. `None` si el grupo no está.
fn group_gid(name: &str) -> Option<u32> {
    let content = std::fs::read_to_string("/etc/group").ok()?;
    for line in content.lines() {
        let mut fields = line.split(':');
        if fields.next() == Some(name) {
            // group: nombre:x:gid:miembros
            return fields.nth(1)?.parse().ok();
        }
    }
    None
}

/// Cliente: se conecta al socket y manda una orden. Lo usan los subcomandos
/// `quit` y `resume`.
pub fn send(path: &str, command: &str) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(Path::new(path))?;
    writeln!(stream, "{command}")?;
    stream.flush()
}
