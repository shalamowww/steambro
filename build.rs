//! Cargo's build script: Tauri's build step, which puts the icons and the UI into the exe.

fn main() {
    // Rebuild the exe when the icons or the UI change.
    println!("cargo:rerun-if-changed=icons");
    println!("cargo:rerun-if-changed=ui");
    tauri_build::build()
}
