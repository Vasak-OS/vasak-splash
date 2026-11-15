//! La implementación real del `Backend` con drm-rs: dumb buffers, sin GBM ni
//! GL. Este archivo es el camino que **no** se puede probar sin hardware —todo
//! lo demás vive detrás de rasgos para probarse con dobles—, así que se mantiene
//! lo más fino posible: abrir la tarjeta, elegir conector y CRTC, crear un dumb
//! buffer, y mapear / publicar. La lógica de decisión (qué GPU, qué conector)
//! está en `select`, que sí se prueba.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};

use drm::buffer::{Buffer, DrmFourcc};
use drm::control::dumbbuffer::DumbBuffer;
use drm::control::{connector, crtc, framebuffer, Device as ControlDevice, Mode};
use drm::Device as BasicDevice;

use crate::run::Backend;
use crate::select::{self, ConnectorCandidate};

/// Envoltura del descriptor de la tarjeta DRM. drm-rs pide sólo que sepa dar su
/// fd; los dos rasgos marcadores le habilitan todas las operaciones de KMS.
struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl BasicDevice for Card {}
impl ControlDevice for Card {}

pub struct DrmBackend {
    card: Card,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    buffer: DumbBuffer,
    framebuffer: framebuffer::Handle,
}

impl DrmBackend {
    /// Abre la GPU primaria, elige el primer conector enchufado con modo y
    /// prepara el dumb buffer y el framebuffer. No toma el DRM todavía: eso lo
    /// hace `acquire`, en el lazo, con reintentos.
    pub fn open() -> io::Result<Self> {
        let card_path = select::primary_card(list_cards()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no hay ningún /dev/dri/card*")
        })?;
        let card = Card(OpenOptions::new().read(true).write(true).open(&card_path)?);

        let resources = card.resource_handles()?;

        // Probar cada conector y quedarse con el primero enchufado y con modo.
        let infos: Vec<connector::Info> = resources
            .connectors()
            .iter()
            .filter_map(|&h| card.get_connector(h, true).ok())
            .collect();
        let candidates: Vec<ConnectorCandidate> = infos
            .iter()
            .map(|info| ConnectorCandidate {
                connected: info.state() == connector::State::Connected,
                has_mode: !info.modes().is_empty(),
            })
            .collect();
        let index = select::pick_connector(&candidates).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "ningún conector enchufado con modo",
            )
        })?;
        let info = &infos[index];
        // El primer modo es el preferido del monitor.
        let mode = info.modes()[0];

        let crtc = pick_crtc(&card, &resources, info)?;

        let (w, h) = mode.size();
        let buffer = card.create_dumb_buffer((w as u32, h as u32), DrmFourcc::Xrgb8888, 32)?;
        let framebuffer = card.add_framebuffer(&buffer, 24, 32)?;

        // El vt pasa a modo gráfico para que fbcon no escriba texto encima del
        // dibujo. Best-effort: si falla, el splash igual dibuja; es un invariante
        // anti-parpadeo a validar en hardware.
        set_graphics_mode();

        Ok(DrmBackend {
            card,
            crtc,
            connector: info.handle(),
            mode,
            buffer,
            framebuffer,
        })
    }
}

impl Backend for DrmBackend {
    fn acquire(&mut self) -> io::Result<()> {
        self.card.acquire_master_lock()
    }

    fn release(&mut self) {
        // En el peor caso el cierre del fd suelta el master igual.
        let _ = self.card.release_master_lock();
    }

    fn with_frame(&mut self, draw: &mut crate::run::DrawFn<'_>) {
        let (w, h) = self.mode.size();
        let pitch = self.buffer.pitch() as usize;
        if let Ok(mut mapping) = self.card.map_dumb_buffer(&mut self.buffer) {
            let bytes: &mut [u8] = &mut mapping;
            draw(bytes, pitch, w as u32, h as u32);
        }
    }

    fn present(&mut self) -> io::Result<()> {
        self.card.set_crtc(
            self.crtc,
            Some(self.framebuffer),
            (0, 0),
            &[self.connector],
            Some(self.mode),
        )
    }
}

/// Lista los nodos `/dev/dri/card*`. El filtrado por nombre deja fuera los
/// `renderD*`, que no sirven para KMS.
fn list_cards() -> Vec<PathBuf> {
    let mut cards = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/dev/dri") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with("card") {
                cards.push(entry.path());
            }
        }
    }
    cards
}

/// Elige un CRTC para el conector: el que ya tenga su encoder, o el primero que
/// permitan los encoders del conector.
fn pick_crtc(
    card: &Card,
    resources: &drm::control::ResourceHandles,
    info: &connector::Info,
) -> io::Result<crtc::Handle> {
    // Si el conector ya está atado a un encoder con CRTC, reusarlo evita un
    // modeset de más.
    if let Some(enc) = info.current_encoder() {
        if let Ok(enc_info) = card.get_encoder(enc) {
            if let Some(crtc) = enc_info.crtc() {
                return Ok(crtc);
            }
        }
    }
    for &enc in info.encoders() {
        if let Ok(enc_info) = card.get_encoder(enc) {
            if let Some(crtc) = resources.filter_crtcs(enc_info.possible_crtcs()).first() {
                return Ok(*crtc);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "el conector no tiene ningún CRTC disponible",
    ))
}

/// Pone el vt en modo gráfico con el ioctl `KDSETMODE` sobre `/dev/tty0` (la
/// consola activa). Todo best-effort: cualquier error se ignora.
fn set_graphics_mode() {
    const KDSETMODE: libc::c_ulong = 0x4B3A;
    const KD_GRAPHICS: libc::c_long = 0x01;
    if let Ok(tty) = OpenOptions::new()
        .read(true)
        .write(true)
        .open(Path::new("/dev/tty0"))
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::ioctl(tty.as_raw_fd(), KDSETMODE, KD_GRAPHICS);
        }
    }
}

/// Devuelve el vt a modo texto con `KDSETMODE`/`KD_TEXT` sobre `/dev/tty0`, el
/// espejo de `set_graphics_mode`. Se llama en los caminos de salida **sin**
/// handoff (GaveUp, TimedOut, y el fallo al abrir el socket) para no dejar la
/// consola en negro ocultando los errores de arranque. En el handoff no se
/// llama: el greeter toma el scanout en modo gráfico. Como `panic = "abort"`
/// impide que un `Drop` corra en el unwind, la restauración es explícita en
/// esos caminos, no vía guard. Best-effort: cualquier error se ignora.
pub fn set_text_mode() {
    const KDSETMODE: libc::c_ulong = 0x4B3A;
    const KD_TEXT: libc::c_long = 0x00;
    if let Ok(tty) = OpenOptions::new()
        .read(true)
        .write(true)
        .open(Path::new("/dev/tty0"))
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::ioctl(tty.as_raw_fd(), KDSETMODE, KD_TEXT);
        }
    }
}
