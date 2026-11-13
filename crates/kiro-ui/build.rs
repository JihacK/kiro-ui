//! rust-embed needs `dist/web` to exist; create a stub page so the crate always compiles.
fn main() {
    println!("cargo:rerun-if-changed=../../dist/web");
    if std::env::var_os("CARGO_FEATURE_SERVER").is_none() {
        return;
    }
    let dir = std::path::Path::new("../../dist/web");
    let index = dir.join("index.html");
    if !index.exists() {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(index, "<!doctype html><title>Kiro</title><p>Browser UI not built. Run: cargo xtask web\n");
    }
}
