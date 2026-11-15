//! El logo embebido como RGBA crudo. `build.rs` lo genera desde
//! `assets/logo.png` y lo deja en `OUT_DIR`; acá sólo se incluye. Expone
//! `LOGO_WIDTH`, `LOGO_HEIGHT` y `LOGO_RGBA`.
include!(concat!(env!("OUT_DIR"), "/logo_data.rs"));
