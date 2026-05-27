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
        format!("#!/bin/sh\ncat > \"{}\"\n", output.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    output
}

/// Runs `yoink __copy <mode> <path> <line>` with `extra_path` prepended to PATH
/// and WAYLAND_DISPLAY removed (so detection always falls through to X11).
fn run_copy(extra_path: &str, mode: &str, path: &str, line: &str) -> std::process::Output {
    let current_path = std::env::var("PATH").unwrap_or_default();
    Command::new(env!("CARGO_BIN_EXE_yoink"))
        .arg("__copy")
        .arg(mode)
        .arg(path)
        .arg(line)
        .env("PATH", format!("{extra_path}:{current_path}"))
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .expect("failed to run yoink __copy")
}

// Group A — basic text building

#[test]
fn relative_path_with_line() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(dir.path().to_str().unwrap(), "relative", "src/foo.rs", "42");
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, "src/foo.rs:42");
}

#[test]
fn relative_path_no_line() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(dir.path().to_str().unwrap(), "relative", "src/foo.rs", "");
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, "src/foo.rs");
}

#[test]
fn filename_with_line() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(
        dir.path().to_str().unwrap(),
        "filename",
        "src/bar/baz.rs",
        "10",
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, "baz.rs:10");
}

#[test]
fn filename_no_line() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(
        dir.path().to_str().unwrap(),
        "filename",
        "src/bar/baz.rs",
        "",
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, "baz.rs");
}

#[test]
fn filename_shallow_path() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(dir.path().to_str().unwrap(), "filename", "baz.rs", "");
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, "baz.rs");
}

// Group D — path format variants

#[test]
fn path_with_spaces_relative() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(
        dir.path().to_str().unwrap(),
        "relative",
        "src/my file.rs",
        "5",
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, "src/my file.rs:5");
}

#[test]
fn path_with_spaces_filename() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(
        dir.path().to_str().unwrap(),
        "filename",
        "src/my file.rs",
        "",
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, "my file.rs");
}

#[test]
fn unicode_path_relative() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(
        dir.path().to_str().unwrap(),
        "relative",
        "src/résumé.rs",
        "",
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, "src/résumé.rs");
}

#[test]
fn deep_nested_filename() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(
        dir.path().to_str().unwrap(),
        "filename",
        "a/b/c/d/e/deep.rs",
        "1",
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, "deep.rs:1");
}

#[test]
fn hidden_file_relative() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(
        dir.path().to_str().unwrap(),
        "relative",
        ".hidden/secret.rs",
        "3",
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, ".hidden/secret.rs:3");
}

#[test]
fn hidden_file_filename() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(
        dir.path().to_str().unwrap(),
        "filename",
        ".hidden/secret.rs",
        "",
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, "secret.rs");
}

#[test]
fn multi_dot_extension() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(
        dir.path().to_str().unwrap(),
        "filename",
        "archive.tar.gz",
        "",
    );
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, "archive.tar.gz");
}

#[test]
fn no_extension_makefile() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(dir.path().to_str().unwrap(), "relative", "Makefile", "");
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, "Makefile");
}

#[test]
fn directory_without_slash() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(dir.path().to_str().unwrap(), "filename", "src/utils", "");
    let content = fs::read_to_string(&captured).unwrap();
    assert_eq!(content, "utils");
}

#[test]
fn directory_trailing_slash() {
    let dir = TempDir::new().unwrap();
    let captured = make_fake_clip(dir.path(), "xclip");
    run_copy(dir.path().to_str().unwrap(), "filename", "src/utils/", "");
    let content = fs::read_to_string(&captured).unwrap();
    // Path::new("src/utils/").file_name() returns Some("utils") on Linux (trailing slash stripped),
    // so the filename mode yields "utils" rather than falling back to the full path.
    assert_eq!(content, "utils");
}
