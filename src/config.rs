//! La configuración del splash: hoy sólo el color de fondo y el tope del
//! temporizador de seguridad. El formato es `clave=valor` por línea, con `#`
//! para comentarios, y las claves que no se conocen se ignoran en silencio —no
//! queremos que un arranque falle por una línea de más en un archivo de /etc.

use std::time::Duration;

/// Color de fondo por omisión: el fondo del esquema de fábrica de VasakOS
/// (`vasak-default`, base de Catppuccin Mocha). Se guarda como `0x00RRGGBB`.
pub const DEFAULT_BACKGROUND: u32 = 0x001e_1e2e;

/// Tope del temporizador de seguridad. Si el `quit` del greeter no llega en este
/// tiempo, el daemon se retira solo para no dejar la pantalla colgada.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Fondo sólido, `0x00RRGGBB`.
    pub background: u32,
    /// Tope del temporizador de seguridad.
    pub timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            background: DEFAULT_BACKGROUND,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

/// Parsea `#RRGGBB` (o `RRGGBB`, con o sin `#`) a `0x00RRGGBB`. Devuelve `None`
/// si no son exactamente seis dígitos hexadecimales.
pub fn parse_color(text: &str) -> Option<u32> {
    let hex = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

/// Construye la configuración a partir del texto del archivo. Las claves
/// desconocidas y las líneas mal formadas se ignoran; lo que se reconoce pisa el
/// valor por omisión.
pub fn parse(text: &str) -> Config {
    let mut config = Config::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "background" => {
                if let Some(color) = parse_color(value) {
                    config.background = color;
                }
            }
            "timeout_ms" => {
                if let Ok(ms) = value.parse::<u64>() {
                    config.timeout = Duration::from_millis(ms);
                }
            }
            _ => {}
        }
    }
    config
}
