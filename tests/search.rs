use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use tempfile::tempdir;
use yoink::search::build_candidates;

#[test]
fn build_candidates_errors_on_invalid_regex() {
    with_system_config(".git/**\nnode_modules/**\n", |_| {
        let dir = tempdir().expect("tempdir");
        // `*` with nothing in front is an invalid PCRE-style atom.
        let err = build_candidates("*", dir.path()).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("invalid regex query"),
            "expected an 'invalid regex query' anyhow context, got: {msg}"
        );
    });
}

#[test]
fn build_candidates_returns_files_for_empty_query() {
    with_system_config(".git/**\nnode_modules/**\n", |_| {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("a.txt"), "alpha\n").expect("write a");
        fs::write(dir.path().join("b.txt"), "beta\n").expect("write b");

        let candidates = build_candidates("", dir.path()).expect("build candidates");
        let names: Vec<String> = candidates
            .iter()
            .map(|c| c.path.to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.txt".to_string()), "got: {names:?}");
        assert!(names.contains(&"b.txt".to_string()), "got: {names:?}");
    });
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn with_system_config(config_content: &str, test_fn: impl FnOnce(&Path)) {
    // Recover from any poisoning so one failing test doesn't cascade into
    // the rest. We don't share mutable state across tests; the mutex only
    // serializes env-var mutation.
    let _guard = env_lock()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let temp_home = tempdir().expect("temp home");
    let config_path = temp_home.path().join(".yoink-config");
    // Pin the legacy regex + case-sensitive behavior these tests were
    // written against; the runtime default is now case-insensitive glob,
    // which would change match semantics out from under them.
    let pinned = format!("search_mode=regex\ncase_sensitive=true\n{config_content}");
    fs::write(&config_path, pinned).expect("write config");

    std::env::set_var("YOINK_CONFIG_PATH", &config_path);
    test_fn(temp_home.path());
    std::env::remove_var("YOINK_CONFIG_PATH");
}

#[test]
fn merges_path_and_content_matches() {
    with_system_config(".git/**\nnode_modules/**\n", |_| {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        fs::write(root.join("example.py"), "print('ejectReasons')\n").expect("write file");
        fs::write(root.join("ejectReasonsList.csv"), "header\n").expect("write file");
        fs::create_dir(root.join("subfolder_ejectReasons")).expect("mkdir");

        let candidates = build_candidates("ejectReasons", root).expect("build candidates");
        let mut seen_example = false;
        let mut seen_csv = false;
        let mut seen_dir = false;

        for candidate in candidates {
            let path = candidate.path.to_string_lossy();
            if path == "example.py" {
                assert!(candidate.content_match);
                seen_example = true;
            }
            if path == "ejectReasonsList.csv" {
                assert!(candidate.path_match);
                seen_csv = true;
            }
            if path == "subfolder_ejectReasons" {
                assert!(candidate.path_match);
                assert!(candidate.is_dir);
                seen_dir = true;
            }
        }

        assert!(seen_example && seen_csv && seen_dir);
    });
}

#[test]
fn skips_hidden_paths_by_default() {
    with_system_config(".git/**\nnode_modules/**\n", |_| {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        fs::create_dir(root.join(".hidden")).expect("mkdir hidden");
        fs::write(root.join(".hidden/secret.txt"), "ejectReasons\n").expect("write hidden");
        fs::write(root.join("visible.txt"), "ejectReasons\n").expect("write visible");

        let candidates = build_candidates("ejectReasons", root).expect("build candidates");
        let paths: Vec<String> = candidates
            .into_iter()
            .map(|candidate| candidate.path.to_string_lossy().to_string())
            .collect();

        assert!(paths.iter().any(|path| path == "visible.txt"));
        assert!(!paths.iter().any(|path| path.contains(".hidden")));
    });
}

#[test]
fn respects_yoinkignore_patterns() {
    with_system_config(".git/**\nnode_modules/**\nignored_dir/**\n", |_| {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        fs::create_dir(root.join("ignored_dir")).expect("mkdir ignored");
        fs::write(root.join("ignored_dir/hit.txt"), "ejectReasons\n").expect("write ignored hit");
        fs::write(root.join("kept.txt"), "ejectReasons\n").expect("write kept");

        let candidates = build_candidates("ejectReasons", root).expect("build candidates");
        let paths: Vec<String> = candidates
            .into_iter()
            .map(|candidate| candidate.path.to_string_lossy().to_string())
            .collect();

        assert!(paths.iter().any(|path| path == "kept.txt"));
        assert!(!paths.iter().any(|path| path.starts_with("ignored_dir/")));
    });
}

#[test]
fn applies_configured_ignore_globs() {
    with_system_config(".git/**\nnode_modules/**\n", |_| {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        fs::create_dir(root.join(".git")).expect("mkdir git");
        fs::write(root.join(".git/ignored.txt"), "ejectReasons\n").expect("write git ignored");
        fs::create_dir(root.join("node_modules")).expect("mkdir node_modules");
        fs::write(root.join("node_modules/ignored.txt"), "ejectReasons\n")
            .expect("write node_modules ignored");
        fs::write(root.join("kept.txt"), "ejectReasons\n").expect("write kept");

        let candidates = build_candidates("ejectReasons", root).expect("build candidates");
        let paths: Vec<String> = candidates
            .into_iter()
            .map(|candidate| candidate.path.to_string_lossy().to_string())
            .collect();

        assert!(paths.iter().any(|path| path == "kept.txt"));
        assert!(!paths.iter().any(|path| path.starts_with(".git/")));
        assert!(!paths.iter().any(|path| path.starts_with("node_modules/")));
    });
}

#[test]
fn allows_hidden_when_toggle_enabled() {
    with_system_config("include_hidden=true\n.git/**\nnode_modules/**\n", |_| {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        fs::create_dir(root.join(".hidden")).expect("mkdir hidden");
        fs::write(root.join(".hidden/secret.txt"), "ejectReasons\n").expect("write hidden");

        let candidates = build_candidates("ejectReasons", root).expect("build candidates");
        let paths: Vec<String> = candidates
            .into_iter()
            .map(|candidate| candidate.path.to_string_lossy().to_string())
            .collect();

        assert!(paths.iter().any(|path| path == ".hidden/secret.txt"));
    });
}

#[test]
fn sorts_by_depth_then_alphabetical() {
    with_system_config(".git/**\nnode_modules/**\n", |_| {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        fs::create_dir_all(root.join("a/deeper")).expect("mkdir a/deeper");
        fs::create_dir_all(root.join("b/deeper")).expect("mkdir b/deeper");
        fs::write(root.join("z_root.txt"), "root\n").expect("write z_root");
        fs::write(root.join("a/deeper/file1.txt"), "x\n").expect("write file1");
        fs::write(root.join("b/deeper/file2.txt"), "x\n").expect("write file2");
        fs::write(root.join("a_root.txt"), "root\n").expect("write a_root");

        let candidates = build_candidates("", root).expect("build candidates");
        let paths: Vec<String> = candidates
            .into_iter()
            .map(|candidate| candidate.path.to_string_lossy().to_string())
            .collect();

        let a_root_idx = paths
            .iter()
            .position(|path| path == "a_root.txt")
            .expect("a_root index");
        let z_root_idx = paths
            .iter()
            .position(|path| path == "z_root.txt")
            .expect("z_root index");
        let deep_idx = paths
            .iter()
            .position(|path| path == "a/deeper/file1.txt")
            .expect("deep index");

        assert!(a_root_idx < z_root_idx);
        assert!(z_root_idx < deep_idx);
    });
}

#[test]
fn sorts_alphabetically_when_configured() {
    with_system_config("sort=alphabetical\n.git/**\nnode_modules/**\n", |_| {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        fs::create_dir_all(root.join("a/deeper")).expect("mkdir a/deeper");
        fs::write(root.join("z_root.txt"), "root\n").expect("write z_root");
        fs::write(root.join("a/deeper/file1.txt"), "x\n").expect("write file1");
        fs::write(root.join("a_root.txt"), "root\n").expect("write a_root");

        let candidates = build_candidates("", root).expect("build candidates");
        let paths: Vec<String> = candidates
            .into_iter()
            .map(|candidate| candidate.path.to_string_lossy().to_string())
            .collect();

        let a_deep_idx = paths
            .iter()
            .position(|path| path == "a/deeper/file1.txt")
            .expect("a/deep index");
        let a_root_idx = paths
            .iter()
            .position(|path| path == "a_root.txt")
            .expect("a_root index");

        assert!(a_deep_idx < a_root_idx);
    });
}
