use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn make_fake_clip(dir: &Path, cmd_name: &str) -> std::path::PathBuf {
    let output = dir.join(format!("{cmd_name}.captured"));
    let script = dir.join(cmd_name);
    // Use the absolute path to `cat` so the script works even when PATH is restricted
    // to just the temp dir (which is required by tests that hide system tools like wl-copy).
    fs::write(
        &script,
        format!("#!/bin/sh\n/usr/bin/cat > \"{}\"\n", output.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    output
}

/// Makes a fake clipboard script that always exits 1 (simulates failure).
fn make_failing_clip(dir: &Path, cmd_name: &str) {
    let script = dir.join(cmd_name);
    fs::write(&script, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
}

fn run_copy(
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

// C1 — clipboard command exits non-zero; yoink must still exit 0 and print to stderr
#[test]
fn clipboard_command_exits_nonzero() {
    let dir = TempDir::new().unwrap();
    make_failing_clip(dir.path(), "xclip");
    let system_path = std::env::var("PATH").unwrap_or_default();
    let path_val = format!("{}:{}", dir.path().display(), system_path);
    let output = run_copy(&path_val, None, "relative", "src/foo.rs", "");
    assert!(
        output.status.success(),
        "expected exit code 0, got {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("yoink copy error:"),
        "expected 'yoink copy error:' in stderr, got: {stderr}"
    );
}

// E1 — WAYLAND_DISPLAY is set but wl-copy is absent; should fall through to xclip.
//      Use only the temp dir as PATH so the real system wl-copy (if installed) is hidden.
#[test]
fn wayland_set_but_wl_copy_absent_falls_through_to_xclip() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    // PATH contains only the temp dir — wl-copy is intentionally not present there,
    // so detect_clipboard falls through to the fake xclip.
    let path_val = dir.path().to_str().unwrap().to_string();
    let output = run_copy(&path_val, Some(":0"), "relative", "src/foo.rs", "");
    assert!(
        output.status.success(),
        "expected exit code 0, got {:?}",
        output.status
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert!(
        content.contains("src/foo.rs"),
        "expected 'src/foo.rs' in xclip.captured, got: {content}"
    );
}

// E2 — WAYLAND_DISPLAY set to empty string; wl-copy path is tried (is_some() true for "")
//      but wl-copy is absent (PATH is only temp dir), so falls through to xclip.
#[test]
fn wayland_display_empty_string_falls_through_to_xclip() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    // PATH contains only the temp dir — wl-copy is intentionally not present there.
    let path_val = dir.path().to_str().unwrap().to_string();
    // Explicitly set WAYLAND_DISPLAY to empty string — var_os returns Some("") so is_some() is true
    let output = run_copy(&path_val, Some(""), "relative", "src/foo.rs", "");
    assert!(
        output.status.success(),
        "expected exit code 0, got {:?}",
        output.status
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert!(
        content.contains("src/foo.rs"),
        "expected 'src/foo.rs' in xclip.captured, got: {content}"
    );
}

// E3 — all clipboard tools absent; error message printed, exit code 0
#[test]
fn all_tools_absent_prints_error_exits_zero() {
    let dir = TempDir::new().unwrap();
    // PATH is only the empty temp dir — no real tools visible
    let path_val = dir.path().to_str().unwrap().to_string();
    let output = run_copy(&path_val, None, "relative", "src/foo.rs", "");
    assert!(
        output.status.success(),
        "expected exit code 0, got {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no clipboard tool found"),
        "expected 'no clipboard tool found' in stderr, got: {stderr}"
    );
}

// E4 — variant of C1 using xsel: failing xsel exits non-zero, yoink still exits 0
#[test]
fn clipboard_command_exits_nonzero_shows_error() {
    let dir = TempDir::new().unwrap();
    make_failing_clip(dir.path(), "xsel");
    // Only temp dir in PATH so only xsel is found (no xclip, no wl-copy)
    let path_val = dir.path().to_str().unwrap().to_string();
    let output = run_copy(&path_val, None, "relative", "src/foo.rs", "");
    assert!(
        output.status.success(),
        "expected exit code 0, got {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("yoink copy error:"),
        "expected 'yoink copy error:' in stderr, got: {stderr}"
    );
}

// E5 — clipboard tool exits 0 without reading stdin; not a fatal error
#[test]
fn clipboard_stdin_closes_early() {
    let dir = TempDir::new().unwrap();
    // Script immediately exits 0 without reading stdin
    let script = dir.path().join("xclip");
    fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let system_path = std::env::var("PATH").unwrap_or_default();
    let path_val = format!("{}:{}", dir.path().display(), system_path);
    let output = run_copy(&path_val, None, "relative", "src/foo.rs", "");
    assert!(
        output.status.success(),
        "expected exit code 0, got {:?}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

// E6 — empty path argument copies empty string to clipboard
#[test]
fn empty_path_copies_empty_string() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    let system_path = std::env::var("PATH").unwrap_or_default();
    let path_val = format!("{}:{}", dir.path().display(), system_path);
    let output = run_copy(&path_val, None, "relative", "", "");
    assert!(
        output.status.success(),
        "expected exit code 0, got {:?}",
        output.status
    );
    assert!(
        captured.exists(),
        "expected xclip.captured to exist (clipboard was invoked)"
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, "", "expected empty content, got: {content:?}");
}

// E7 — unrecognised mode uses path as-is (the else branch in main.rs)
#[test]
fn unrecognised_mode_copies_path_as_is() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    let system_path = std::env::var("PATH").unwrap_or_default();
    let path_val = format!("{}:{}", dir.path().display(), system_path);
    let output = run_copy(&path_val, None, "absolute", "src/foo.rs", "");
    assert!(
        output.status.success(),
        "expected exit code 0, got {:?}",
        output.status
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert!(
        content.contains("src/foo.rs"),
        "expected 'src/foo.rs' in xclip.captured, got: {content}"
    );
}

// E8 — non-numeric line string is appended literally
#[test]
fn non_numeric_line_string_appended_literally() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    let system_path = std::env::var("PATH").unwrap_or_default();
    let path_val = format!("{}:{}", dir.path().display(), system_path);
    let output = run_copy(&path_val, None, "relative", "src/foo.rs", "abc");
    assert!(
        output.status.success(),
        "expected exit code 0, got {:?}",
        output.status
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(
        content, "src/foo.rs:abc",
        "expected 'src/foo.rs:abc', got: {content:?}"
    );
}

// E9 — path containing shell metacharacters is passed as a literal argument
#[test]
fn path_with_shell_metacharacters_passed_as_literal() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    let system_path = std::env::var("PATH").unwrap_or_default();
    let path_val = format!("{}:{}", dir.path().display(), system_path);
    // Command::new does not use a shell, so $() is never interpreted
    let output = run_copy(&path_val, None, "relative", "src/foo$(bar).rs", "");
    assert!(
        output.status.success(),
        "expected exit code 0, got {:?}",
        output.status
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(
        content, "src/foo$(bar).rs",
        "expected literal path with metacharacters, got: {content:?}"
    );
}
