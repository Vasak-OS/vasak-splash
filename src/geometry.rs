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
