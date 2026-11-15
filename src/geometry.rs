//! Geometría del blit: dónde cae el logo sobre la pantalla. Separado del
//! dibujo para poder probarlo sin un framebuffer.

/// Esquina superior izquierda donde apoyar un elemento de tamaño `item` para que
/// quede centrado en una pantalla de tamaño `screen`. El resultado es con signo
/// a propósito: si el elemento es más grande que la pantalla, el desplazamiento
/// es negativo y el dibujo lo recorta (ver `render::blit_rgba`), en vez de
/// moverlo a una esquina.
pub fn center(screen: (u32, u32), item: (u32, u32)) -> (i64, i64) {
    let x = (screen.0 as i64 - item.0 as i64) / 2;
    let y = (screen.1 as i64 - item.1 as i64) / 2;
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_logo_se_centra_en_la_pantalla() {
        assert_eq!(center((1920, 1080), (200, 100)), (860, 490));
    }

    #[test]
    fn un_logo_del_tamano_de_la_pantalla_queda_en_el_origen() {
        assert_eq!(center((800, 600), (800, 600)), (0, 0));
    }

    #[test]
    fn un_logo_mas_grande_que_la_pantalla_da_desplazamiento_negativo() {
        assert_eq!(center((100, 100), (300, 200)), (-100, -50));
    }

    #[test]
    fn con_diferencia_impar_el_centro_se_redondea_hacia_cero() {
        assert_eq!(center((101, 101), (100, 100)), (0, 0));
        assert_eq!(center((100, 100), (101, 101)), (0, 0));
        assert_eq!(center((103, 103), (100, 100)), (1, 1));
    }

    #[test]
    fn los_tamanos_cero_no_hacen_panico() {
        assert_eq!(center((0, 0), (0, 0)), (0, 0));
        assert_eq!(center((1920, 1080), (0, 0)), (960, 540));
        assert_eq!(center((0, 0), (10, 20)), (-5, -10));
    }

    #[test]
    fn los_extremos_de_u32_no_desbordan() {
        let (x, y) = center((0, u32::MAX), (u32::MAX, 0));
        assert_eq!(x, -(u32::MAX as i64) / 2);
        assert_eq!(y, u32::MAX as i64 / 2);
    }

    #[test]
    fn los_ejes_son_independientes() {
        assert_eq!(center((1000, 100), (100, 300)), (450, -100));
    }
}
