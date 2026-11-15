// El logo se decodifica una sola vez, acá, y se guarda como RGBA crudo en el
// directorio de compilación. El binario lo embebe con `include_bytes!`, de modo
// que en ejecución no se decodifica nada ni se arrastra ninguna dependencia de
// imagen: un `memcpy` y nada más.
use std::{env, fs, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=assets/logo.png");

    let png_bytes = fs::read("assets/logo.png").expect("falta assets/logo.png");
    // png 0.18 pide `BufRead + Seek`; un `Cursor` sobre el Vec los da.
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let mut reader = decoder
        .read_info()
        .expect("assets/logo.png no es un PNG válido");

    let mut buf = vec![
        0u8;
        reader
            .output_buffer_size()
            .expect("tamaño de buffer del PNG")
    ];
    let info = reader
        .next_frame(&mut buf)
        .expect("no se pudo decodificar assets/logo.png");

    // El logo del taller ya es RGBA de 8 bits; si alguien lo reemplaza por otra
    // cosa, cortar acá es mejor que dibujar basura en el arranque.
    assert_eq!(
        info.color_type,
        png::ColorType::Rgba,
        "el logo tiene que ser RGBA de 8 bits"
    );
    assert_eq!(info.bit_depth, png::BitDepth::Eight);

    let rgba = &buf[..info.buffer_size()];
    let out_dir = env::var("OUT_DIR").expect("cargo no pasó OUT_DIR");
    fs::write(Path::new(&out_dir).join("logo.rgba"), rgba).unwrap();
    fs::write(
        Path::new(&out_dir).join("logo_data.rs"),
        format!(
            "pub const LOGO_WIDTH: u32 = {};\n\
             pub const LOGO_HEIGHT: u32 = {};\n\
             pub static LOGO_RGBA: &[u8] = \
             include_bytes!(concat!(env!(\"OUT_DIR\"), \"/logo.rgba\"));\n",
            info.width, info.height
        ),
    )
    .unwrap();
}
