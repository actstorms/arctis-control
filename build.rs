fn main() {
    // Assets are embedded at compile time; without these hints cargo keeps a
    // stale bundle when only the UI changes.
    println!("cargo:rerun-if-changed=ui");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    tauri_build::build()
}
