use std::path::Path;

use yoink::actions::resolve_target_dir;

#[test]
fn resolve_target_dir_for_file() {
    let cwd = Path::new("/tmp/work");
    let target = resolve_target_dir(cwd, "src/main.rs");
    assert_eq!(target, Path::new("/tmp/work/src"));
}

#[test]
fn resolve_target_dir_for_directory() {
    let cwd = Path::new("/tmp/work");
    let target = resolve_target_dir(cwd, "src");
    assert_eq!(target, Path::new("/tmp/work"));
}
