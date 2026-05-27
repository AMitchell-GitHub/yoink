use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Creates a fake clipboard script named `cmd_name` in `dir`.
/// The script accepts any args and writes its stdin to `<cmd_name>.captured` in the same dir.
/// Returns the path to the captured-output file.
fn make_fake_clip(dir: &Path, cmd_name: &str) -> std::path::PathBuf {
    let output = dir.join(format!("{cmd_name}.captured"));
    let script = dir.join(cmd_name);
    fs::write(
        &script,
        format!("#!/bin/sh\n/usr/bin/cat > \"{}\"\n", output.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    output
}

fn run_copy_with_env(
    path_val: &str,
    wayland_display: Option<&str>,
    mode: &str,
    path: &str,
    line: &str,
) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_yoink"));
    cmd.arg("__copy")
        .arg(mode)
        .arg(path)
        .arg(line)
        .env("PATH", path_val)
        .env_remove("WAYLAND_DISPLAY");
    if let Some(wd) = wayland_display {
        cmd.env("WAYLAND_DISPLAY", wd);
    }
    cmd.output().expect("failed to run yoink __copy")
}

#[test]
fn wayland_wins_over_xclip() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let wl_captured = make_fake_clip(dir, "wl-copy");
    let xclip_captured = make_fake_clip(dir, "xclip");

    let system_path = std::env::var("PATH").unwrap_or_default();
    let path_val = format!("{}:{}", dir.display(), system_path);

    let output = run_copy_with_env(&path_val, Some(":0"), "relative", "src/foo.rs", "");
    assert!(output.status.success(), "expected exit 0, stderr: {}", String::from_utf8_lossy(&output.stderr));

    let wl_content = fs::read_to_string(&wl_captured).expect("wl-copy.captured should exist");
    assert!(wl_content.contains("src/foo.rs"), "wl-copy should have received src/foo.rs, got: {wl_content:?}");

    assert!(!xclip_captured.exists(), "xclip.captured should NOT exist when wl-copy is preferred");
}

#[test]
fn xclip_used_when_no_wayland() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let xclip_captured = make_fake_clip(dir, "xclip");

    let system_path = std::env::var("PATH").unwrap_or_default();
    let path_val = format!("{}:{}", dir.display(), system_path);

    let output = run_copy_with_env(&path_val, None, "relative", "src/foo.rs", "");
    assert!(output.status.success(), "expected exit 0, stderr: {}", String::from_utf8_lossy(&output.stderr));

    let xclip_content = fs::read_to_string(&xclip_captured).expect("xclip.captured should exist");
    assert!(xclip_content.contains("src/foo.rs"), "xclip should have received src/foo.rs, got: {xclip_content:?}");
}

#[test]
fn xsel_fallback_when_no_xclip() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let xsel_captured = make_fake_clip(dir, "xsel");

    let tempdir_str = dir.to_str().unwrap();

    let output = run_copy_with_env(tempdir_str, None, "relative", "src/foo.rs", "");
    assert!(output.status.success(), "expected exit 0, stderr: {}", String::from_utf8_lossy(&output.stderr));

    let xsel_content = fs::read_to_string(&xsel_captured).expect("xsel.captured should exist");
    assert!(xsel_content.contains("src/foo.rs"), "xsel should have received src/foo.rs, got: {xsel_content:?}");
}

#[test]
fn error_when_no_clipboard_tools_available() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let tempdir_str = dir.to_str().unwrap();

    let output = run_copy_with_env(tempdir_str, None, "relative", "src/foo.rs", "");
    assert_eq!(output.status.code(), Some(0), "expected exit code 0 (non-fatal error)");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no clipboard tool found"), "expected 'no clipboard tool found' in stderr, got: {stderr:?}");
}
