fn main() {
    // The icons and the UI are built into the exe; rebuild it when they change.
    println!("cargo:rerun-if-changed=icons");
    println!("cargo:rerun-if-changed=ui");
    tauri_build::build()
}
