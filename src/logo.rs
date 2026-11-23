//! El logo embebido como RGBA crudo. `build.rs` lo genera desde
//! `assets/logo.png` y lo deja en `OUT_DIR`; acá sólo se incluye. Expone
//! `LOGO_WIDTH`, `LOGO_HEIGHT` y `LOGO_RGBA`.
include!(concat!(env!("OUT_DIR"), "/logo_data.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    /// (opacos, de esos con el verde dominando) sobre una grilla de 1 de cada 4
    /// píxeles de toda la imagen.
    fn green_share() -> (usize, usize) {
        let w = LOGO_WIDTH as usize;
        let mut opaque = 0;
        let mut green = 0;
        for (i, px) in LOGO_RGBA.chunks_exact(4).enumerate() {
            if !(i % w).is_multiple_of(4) || !(i / w).is_multiple_of(4) || px[3] < 200 {
                continue;
            }
            opaque += 1;
            if px[1] > px[0] && px[1] > px[2] {
                green += 1;
            }
        }
        (opaque, green)
    }

    #[test]
    fn el_logo_embebido_tiene_el_tamano_y_los_bytes_que_declara() {
        assert_eq!((LOGO_WIDTH, LOGO_HEIGHT), (512, 512));
        assert_eq!(LOGO_RGBA.len(), (LOGO_WIDTH * LOGO_HEIGHT * 4) as usize);
    }

    #[test]
    fn el_logo_embebido_es_el_de_vasakos_y_no_el_de_tauri() {
        let (opaque, green) = green_share();
        eprintln!("opacos={opaque} verdes={green}");
        assert!(opaque > 1000, "el logo casi no tiene píxeles opacos");
        // El degradé verde de VasakOS domina; el logo de Tauri (amarillo y cian)
        // casi no tiene píxeles donde G sea el mayor canal estricto.
        assert!(
            green * 100 / opaque >= 50,
            "el verde no domina el logo: {green} de {opaque} píxeles opacos"
        );
    }
}
