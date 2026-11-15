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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_texto_vacio_da_la_configuracion_por_omision() {
        assert_eq!(parse(""), Config::default());
        assert_eq!(parse("\n\n  \n"), Config::default());
    }

    #[test]
    fn los_valores_validos_pisan_los_de_omision() {
        let c = parse("background=#112233\ntimeout_ms=1500\n");
        assert_eq!(c.background, 0x0011_2233);
        assert_eq!(c.timeout, Duration::from_millis(1500));
    }

    #[test]
    fn los_espacios_alrededor_de_clave_y_valor_se_ignoran() {
        let c = parse("  background  =  ff0000  \n\ttimeout_ms = 5 ");
        assert_eq!(c.background, 0x00ff_0000);
        assert_eq!(c.timeout, Duration::from_millis(5));
    }

    #[test]
    fn los_comentarios_y_las_claves_desconocidas_se_ignoran() {
        let c = parse("# background=#ffffff\nlogo=grande\ntimeout_ms=100\n");
        assert_eq!(c.background, DEFAULT_BACKGROUND);
        assert_eq!(c.timeout, Duration::from_millis(100));
    }

    #[test]
    fn las_lineas_sin_igual_se_ignoran() {
        assert_eq!(parse("background\nbasura sin igual"), Config::default());
    }

    #[test]
    fn un_color_malformado_conserva_el_de_omision() {
        for malo in [
            "", "#12345", "#1234567", "gggggg", "#12 456", "##123456", "+12345",
        ] {
            let c = parse(&format!("background={malo}"));
            assert_eq!(c.background, DEFAULT_BACKGROUND, "entrada: {malo:?}");
        }
    }

    #[test]
    fn un_timeout_invalido_conserva_el_de_omision() {
        for malo in ["", "abc", "-1", "1.5", "99999999999999999999999"] {
            let c = parse(&format!("timeout_ms={malo}"));
            assert_eq!(c.timeout, DEFAULT_TIMEOUT, "entrada: {malo:?}");
        }
    }

    #[test]
    fn un_valor_invalido_no_afecta_a_las_demas_claves() {
        let c = parse("background=zzz\ntimeout_ms=250");
        assert_eq!(c.background, DEFAULT_BACKGROUND);
        assert_eq!(c.timeout, Duration::from_millis(250));
    }

    #[test]
    fn si_una_clave_se_repite_gana_la_ultima_valida() {
        let c = parse("background=#111111\nbackground=#222222\nbackground=roto");
        assert_eq!(c.background, 0x0022_2222);
    }

    #[test]
    fn el_valor_puede_contener_otro_igual_sin_romper_el_parseo() {
        assert_eq!(parse("background==#123456").background, DEFAULT_BACKGROUND);
    }

    #[test]
    fn parse_color_acepta_con_y_sin_almohadilla_y_mayusculas() {
        assert_eq!(parse_color("#1E1E2E"), Some(0x001e_1e2e));
        assert_eq!(parse_color("1e1e2e"), Some(0x001e_1e2e));
        assert_eq!(parse_color("  #000000 "), Some(0));
        assert_eq!(parse_color("#FFFFFF"), Some(0x00ff_ffff));
    }

    #[test]
    fn parse_color_rechaza_unicode_sin_panico() {
        // Seis bytes que no son seis dígitos hex (dos caracteres de tres bytes).
        assert_eq!(parse_color("日本"), None);
        assert_eq!(parse_color("é"), None);
    }

    #[test]
    fn el_timeout_cero_se_acepta_tal_cual() {
        // Documenta el comportamiento actual: no hay piso para el timeout.
        assert_eq!(parse("timeout_ms=0").timeout, Duration::ZERO);
    }

    #[test]
    fn una_entrada_enorme_no_hace_panico() {
        let basura = "x=".repeat(200_000) + &"\n=\n".repeat(10_000);
        assert_eq!(parse(&basura), Config::default());
    }
}
