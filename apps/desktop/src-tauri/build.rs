//! Build script: delegates to `tauri_build::build()`, which generates the
//! context from tauri.conf.json and the capabilities directory.
fn main() {
    tauri_build::build();
}
