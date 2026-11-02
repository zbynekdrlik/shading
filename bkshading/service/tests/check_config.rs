//! `bkshading --check-config <file>` (shading#1): the interface camera-box uses to prove its
//! generated strih seed loads in the PINNED service release, without starting the service.
//! Contract (docs/CONTRACT.md): exit 0 + `ok: <N> cameras` on stdout when the file loads;
//! non-zero exit + the file path and the reason on stderr when it does not. It never binds a port.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_bkshading")
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn check(path: &Path) -> std::process::Output {
    Command::new(bin())
        .arg("--check-config")
        .arg(path)
        .output()
        .expect("run bkshading --check-config")
}

#[test]
fn a_strih_seed_passes_with_its_camera_count() {
    let out = check(&manifest_dir().join("tests/fixtures/strih-snv-seed.toml"));
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok: 7 cameras");
}

#[test]
fn the_shipped_example_config_passes() {
    let out = check(&manifest_dir().join("bkshading.example.toml"));
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("ok: "));
}

#[test]
fn a_broken_config_fails_naming_the_file() {
    let dir = std::env::temp_dir().join(format!("bkshading-check-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bad = dir.join("bad.toml");
    std::fs::write(&bad, "[[cameras]]\nid = \"cam1\"\n").unwrap();
    let out = check(&bad);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        !out.status.success(),
        "a config missing required camera fields must fail"
    );
    assert!(out.stdout.is_empty(), "no `ok:` line on failure");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("bad.toml"), "stderr must name the file: {err}");
}

#[test]
fn a_missing_file_fails() {
    let out = check(&manifest_dir().join("tests/fixtures/does-not-exist.toml"));
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("does-not-exist.toml"));
}
