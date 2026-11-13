//! Build tasks. `cargo xtask web [--dev]` builds the wasm UI into dist/web;
//! `cargo xtask dist` also builds the release binary that embeds it.

use flate2::{write::GzEncoder, Compression};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const WASM_BINDGEN_FALLBACK: &str = "0.2.130";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("workspace root").to_path_buf()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("web") => web(args.iter().any(|a| a == "--dev")),
        Some("dist") => web(false).and_then(|()| dist()),
        _ => Err("usage: cargo xtask <web [--dev] | dist>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let status = cmd.status().map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{:?} failed ({status})", cmd.get_program()))
    }
}

fn output(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Version of the `wasm-bindgen` crate pinned in Cargo.lock; the CLI must match it.
fn locked_wasm_bindgen() -> String {
    let lock = fs::read_to_string(root().join("Cargo.lock")).unwrap_or_default();
    let mut lines = lock.lines();
    while let Some(l) = lines.next() {
        if l == "name = \"wasm-bindgen\"" {
            if let Some(v) = lines.next().and_then(|v| v.strip_prefix("version = \"")) {
                return v.trim_end_matches('"').to_string();
            }
        }
    }
    WASM_BINDGEN_FALLBACK.into()
}

fn check_tools() -> Result<(), String> {
    let installed = output(Command::new("rustup").args(["target", "list", "--installed"])).unwrap_or_default();
    if !installed.lines().any(|l| l.trim() == "wasm32-unknown-unknown") {
        return Err("missing wasm target. Run: rustup target add wasm32-unknown-unknown".into());
    }
    let want = locked_wasm_bindgen();
    let have = output(Command::new("wasm-bindgen").arg("--version")).unwrap_or_default();
    if have.split_whitespace().nth(1) != Some(want.as_str()) {
        return Err(format!(
            "wasm-bindgen CLI {want} required (found: {}). Run: cargo install wasm-bindgen-cli --version {want} --locked",
            if have.trim().is_empty() { "none" } else { have.trim() }
        ));
    }
    Ok(())
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::best());
    enc.write_all(data).expect("gzip");
    enc.finish().expect("gzip")
}

fn web(dev: bool) -> Result<(), String> {
    check_tools()?;
    let root = root();
    let target_dir = root.join("target/wasm");
    let profile = if dev { "debug" } else { "release" };

    let mut build = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    build
        .current_dir(&root)
        .args(["build", "-p", "kiro-ui-web", "--target", "wasm32-unknown-unknown", "--target-dir"])
        .arg(&target_dir)
        // wasm-bindgen needs the `target_features` section that `strip` removes; it drops the names itself.
        .env("CARGO_PROFILE_RELEASE_STRIP", "false");
    if !dev {
        build.arg("--release");
    }
    run(&mut build)?;

    let wasm = target_dir.join(format!("wasm32-unknown-unknown/{profile}/kiro_ui_web.wasm"));
    let tmp = target_dir.join("bindgen-out");
    let _ = fs::remove_dir_all(&tmp);
    let mut bindgen = Command::new("wasm-bindgen");
    bindgen.args(["--target", "web", "--no-typescript", "--out-dir"]).arg(&tmp);
    if !dev {
        bindgen.args(["--remove-name-section", "--remove-producers-section"]);
    }
    run(bindgen.arg(&wasm))?;

    let bg = tmp.join("kiro_ui_web_bg.wasm");
    if !dev && output(Command::new("wasm-opt").arg("--version")).is_some() {
        run(Command::new("wasm-opt").arg("-Oz").arg(&bg).arg("-o").arg(&bg))?;
    }

    let out = root.join("dist/web");
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let statics = root.join("crates/kiro-ui-web/static");
    for name in ["index.html", "main.js"] {
        fs::copy(statics.join(name), out.join(name)).map_err(|e| format!("{name}: {e}"))?;
    }
    for name in ["kiro_ui_web.js", "kiro_ui_web_bg.wasm"] {
        let raw = fs::read(tmp.join(name)).map_err(|e| format!("{name}: {e}"))?;
        let gz = gzip(&raw);
        fs::write(out.join(format!("{name}.gz")), &gz).map_err(|e| e.to_string())?;
        println!("{name}: {} KiB -> {} KiB gzip", raw.len() / 1024, gz.len() / 1024);
    }
    println!("web UI written to {}", out.display());
    Ok(())
}

fn dist() -> Result<(), String> {
    let root = root();
    run(Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .current_dir(&root)
        .args(["build", "--release", "-p", "kiro-ui", "--features", "server"]))?;
    let bin = root.join("target/release/kiro-ui");
    let size = fs::metadata(&bin).map(|m| m.len()).unwrap_or(0);
    println!("{} ({} KiB)", bin.display(), size / 1024);
    Ok(())
}
