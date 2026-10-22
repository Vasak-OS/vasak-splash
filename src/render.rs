//! El dibujo sobre el dumb buffer. El formato es siempre XRGB8888: en memoria,
//! poco-endian, cada píxel son cuatro bytes `[B, G, R, X]`. Todo lo de acá
//! trabaja sobre un `&mut [u8]` cualquiera, así que se prueba con buffers
//! chiquitos sin tocar DRM.

/// Pinta todo el framebuffer de un color sólido `0x00RRGGBB`.
///
/// `stride` es el ancho de fila en bytes, que no tiene por qué ser `w * 4`: el
/// driver puede pedir un relleno al final de cada fila, y escribir asumiendo
/// `w * 4` pisaría ese relleno o se saldría del buffer.
pub fn fill(buf: &mut [u8], stride: usize, w: u32, h: u32, color: u32) {
    let (b, g, r) = (
        (color & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        ((color >> 16) & 0xff) as u8,
    );
    for y in 0..h as usize {
        let row = &mut buf[y * stride..];
        for x in 0..w as usize {
            let px = &mut row[x * 4..x * 4 + 4];
            px[0] = b;
            px[1] = g;
            px[2] = r;
            px[3] = 0;
        }
    }
}

/// Compone un logo RGBA sobre el framebuffer con la esquina en `(ox, oy)`,
/// mezclando por alfa (`out = src*a + dst*(1-a)`). Recorta contra los bordes de
/// la pantalla, así que un `(ox, oy)` negativo o un logo más grande que la
/// pantalla no se salen del buffer.
#[allow(clippy::too_many_arguments)]
pub fn blit_rgba(
    buf: &mut [u8],
    stride: usize,
    screen_w: u32,
    screen_h: u32,
    logo: &[u8],
    logo_w: u32,
    logo_h: u32,
    ox: i64,
    oy: i64,
) {
    for ly in 0..logo_h as i64 {
        let dy = oy + ly;
        if dy < 0 || dy >= screen_h as i64 {
            continue;
        }
        for lx in 0..logo_w as i64 {
            let dx = ox + lx;
            if dx < 0 || dx >= screen_w as i64 {
                continue;
            }
            let src = &logo[((ly * logo_w as i64 + lx) * 4) as usize..][..4];
            let a = src[3] as u32;
            if a == 0 {
                continue;
            }
            let dst = &mut buf[dy as usize * stride + dx as usize * 4..][..4];
            // src viene en RGBA; el framebuffer en [B, G, R, X].
            dst[0] = over(src[2], dst[0], a);
            dst[1] = over(src[1], dst[1], a);
            dst[2] = over(src[0], dst[2], a);
            dst[3] = 0;
        }
    }
}

/// Mezcla un canal de origen sobre uno de destino con opacidad `a` (0..=255),
/// redondeando. Con `a == 255` da exactamente `src`; con `a == 0`, `dst`.
fn over(src: u8, dst: u8, a: u32) -> u8 {
    ((src as u32 * a + dst as u32 * (255 - a) + 127) / 255) as u8
}
