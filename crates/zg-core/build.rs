//! Stages `libzvec_c_api` next to the built binaries so they run standalone.
//!
//! Background: `zvec-rust-sys` links `libzvec_c_api` as a `dylib` out of its
//! own `OUT_DIR` (`target/<profile>/build/zvec-rust-sys-*/out/zvec-prebuilt/`)
//! and emits an absolute `-rpath` for it, but that rpath demonstrably never
//! reaches the final artifacts (`readelf -d target/release/zg` shows
//! `RUNPATH=[$ORIGIN]` only). `cargo run` / `cargo test` still work because
//! cargo injects `LD_LIBRARY_PATH` itself when launching targets — direct
//! execution (`./target/release/zg`) does not, hence the manual export.
//!
//! This script closes that gap without baking host-absolute paths into the
//! binary: it copies the newest available shared library into the profile
//! output directory (`target/<profile>/`, i.e. next to `zg`), where the
//! `$ORIGIN` rpath from `.cargo/config.toml` resolves it on Linux. The copy
//! refreshes on every rebuild, so `zvec-rust-sys` build-hash churn (several
//! stale `build/zvec-rust-sys-*` dirs normally accumulate) can never leave a
//! stale pointer behind. When the library cannot be found the script is a
//! silent no-op and linking/reporting stays upstream's job.

// Build scripts talk to cargo via stdout (`cargo::...` directives); that is
// the protocol, not program output.
#![allow(clippy::print_stdout)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn main() {
    println!("cargo::rerun-if-env-changed=ZVEC_LIB_DIR");

    let Some(lib_file) = lib_filename() else {
        return;
    };
    let Some(profile_dir) = profile_dir() else {
        return;
    };
    let Some(source) = find_lib(&profile_dir, lib_file) else {
        return;
    };

    println!("cargo::rerun-if-changed={}", source.display());
    stage_lib(&source, &profile_dir.join(lib_file));
}

/// Shared-library filename for this target; `None` where the layout is unknown.
fn lib_filename() -> Option<&'static str> {
    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("windows") => Some("zvec_c_api.dll"),
        Ok("macos") => Some("libzvec_c_api.dylib"),
        Ok("linux") => Some("libzvec_c_api.so"),
        _ => None,
    }
}

/// `<target>/<profile>/` derived from our own `OUT_DIR`
/// (`.../<profile>/build/<pkg>-<hash>/out`).
fn profile_dir() -> Option<PathBuf> {
    let out_dir = env::var("OUT_DIR").ok()?;
    Path::new(&out_dir)
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
}

/// Locate the library: explicit `ZVEC_LIB_DIR` override first (same precedence
/// as upstream), otherwise the newest copy under the `zvec-rust-sys` build
/// outputs for this profile.
fn find_lib(profile_dir: &Path, lib_file: &str) -> Option<PathBuf> {
    if let Ok(dir) = env::var("ZVEC_LIB_DIR") {
        let candidate = Path::new(&dir).join(lib_file);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    let build_dir = profile_dir.join("build");
    let entries = fs::read_dir(&build_dir).ok()?;

    let mut candidates: Vec<(Option<SystemTime>, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("zvec-rust-sys-") {
            continue;
        }
        let candidate = entry
            .path()
            .join("out")
            .join("zvec-prebuilt")
            .join(lib_file);
        if !candidate.is_file() {
            continue;
        }
        let modified = fs::metadata(&candidate)
            .ok()
            .and_then(|m| m.modified().ok());
        candidates.push((modified, candidate));
    }
    candidates.sort();
    candidates.pop().map(|(_, path)| path)
}

/// Copy the library next to the binaries, skipping the copy when the
/// destination already matches by size (the prebuilt artifact is immutable
/// per version, so equal size means identical content).
fn stage_lib(source: &Path, dest: &Path) {
    let same = fs::metadata(source)
        .ok()
        .and_then(|src| fs::metadata(dest).ok().map(|dst| src.len() == dst.len()))
        .unwrap_or(false);
    if same {
        return;
    }
    if let Err(e) = fs::copy(source, dest) {
        println!(
            "cargo::warning=failed to stage {} next to binaries: {e}",
            source.display()
        );
    }
}
