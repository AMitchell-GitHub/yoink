// Integration tests for src/blame.rs. Each test creates a real git
// repository in a temp directory and exercises the public blame APIs
// against it, plus the no-git error paths.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use tempfile::tempdir;
use yoink::blame::{
    blame_for_file, blame_for_file_cached, blame_for_line_cached, blame_sort_active,
    clear_session_cache, file_last_touched, find_repo_root, format_unix_date,
    latest_change_from_map, line_summary_from_map, session_cache_dir, state_file_path,
    toggle_blame_sort, try_blame_from_cache, BLAME_SORT_ENV, SESSION_CACHE_ENV,
};

// All blame tests touch the process environment (cache dir + state file env
// vars), so they must be serialized to avoid race conditions.
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    cache_dir: PathBuf,
    state_file: PathBuf,
}

impl EnvGuard {
    fn new() -> Self {
        let lock = env_lock().lock().expect("env lock");
        let temp = tempdir().expect("tempdir").into_path();
        let cache_dir = temp.join("cache");
        let state_file = temp.join("state");
        std::env::set_var(SESSION_CACHE_ENV, &cache_dir);
        std::env::set_var(BLAME_SORT_ENV, &state_file);
        Self {
            _lock: lock,
            cache_dir,
            state_file,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::remove_var(SESSION_CACHE_ENV);
        std::env::remove_var(BLAME_SORT_ENV);
        let _ = fs::remove_dir_all(&self.cache_dir);
        let _ = fs::remove_file(&self.state_file);
    }
}

// Create a real git repo with a couple of commits so blame has something to
// say. The author/date are fixed so test assertions can be deterministic.
fn make_repo(root: &Path) {
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "Test Author")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test Author")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("GIT_AUTHOR_DATE", "2024-03-11T12:00:00+0000")
            .env("GIT_COMMITTER_DATE", "2024-03-11T12:00:00+0000")
            .status()
            .expect("git command failed to spawn");
        assert!(status.success(), "git {args:?} failed in {root:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    fs::write(root.join("hello.txt"), "first line\nsecond line\n").expect("write hello");
    git(&["add", "hello.txt"]);
    git(&["commit", "-q", "-m", "initial"]);
}

// ---------------------------------------------------------------------------
// find_repo_root
// ---------------------------------------------------------------------------

#[test]
fn find_repo_root_locates_dotgit_in_ancestor() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir(root.join(".git")).expect("mkdir .git");
    fs::create_dir_all(root.join("a/b/c")).expect("mkdir nested");

    let found = find_repo_root(&root.join("a/b/c"));
    assert_eq!(found.as_deref(), Some(root));
}

#[test]
fn find_repo_root_returns_none_when_no_git_anywhere() {
    let dir = tempdir().expect("tempdir");
    let nested = dir.path().join("a/b/c");
    fs::create_dir_all(&nested).expect("mkdir nested");

    assert!(find_repo_root(&nested).is_none());
}

#[test]
fn find_repo_root_handles_dotgit_as_a_file() {
    // Submodules and worktrees use a `.git` *file*, not a directory.
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(root.join(".git"), "gitdir: /nowhere\n").expect("write .git file");

    let found = find_repo_root(root);
    assert_eq!(found.as_deref(), Some(root));
}

// ---------------------------------------------------------------------------
// format_unix_date — pure function, no env required
// ---------------------------------------------------------------------------

#[test]
fn format_unix_date_known_values() {
    // 0 = 1970-01-01
    assert_eq!(format_unix_date(0), "1970-01-01");
    // 2024-03-11 12:00 UTC = 1710158400
    assert_eq!(format_unix_date(1_710_158_400), "2024-03-11");
    // Far future: 2099-12-31 = 4_102_444_800
    assert_eq!(format_unix_date(4_102_444_800), "2100-01-01");
}

#[test]
fn format_unix_date_handles_negative_timestamps() {
    // Pre-epoch dates (rare but valid). Should not panic; result is a
    // syntactically valid date string.
    let formatted = format_unix_date(-100_000_000);
    assert_eq!(formatted.len(), 10);
    assert!(formatted.starts_with("19"));
}

// ---------------------------------------------------------------------------
// session_cache_dir / state_file_path / toggle / blame_sort_active
// ---------------------------------------------------------------------------

#[test]
fn session_cache_dir_returns_env_var_value_when_set() {
    let guard = EnvGuard::new();
    assert_eq!(session_cache_dir(), Some(guard.cache_dir.clone()));
}

#[test]
fn state_file_path_returns_env_var_value_when_set() {
    let guard = EnvGuard::new();
    assert_eq!(state_file_path(), Some(guard.state_file.clone()));
}

#[test]
fn blame_sort_active_reflects_state_file_existence() {
    let _guard = EnvGuard::new();
    assert!(!blame_sort_active(), "fresh env should start inactive");

    let entered = toggle_blame_sort().expect("toggle on");
    assert!(entered);
    assert!(blame_sort_active(), "active after first toggle");

    let exited = toggle_blame_sort().expect("toggle off");
    assert!(!exited);
    assert!(!blame_sort_active(), "inactive after second toggle");
}

#[test]
fn clear_session_cache_is_a_noop_when_dir_missing() {
    let _guard = EnvGuard::new();
    // No files created — should not panic.
    clear_session_cache();
}

// ---------------------------------------------------------------------------
// blame_for_file / blame_for_file_cached against a real repo
// ---------------------------------------------------------------------------

#[test]
fn blame_for_file_returns_per_line_info() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    make_repo(root);

    let map = blame_for_file(root, Path::new("hello.txt"));
    assert!(map.contains_key(&1), "line 1 should be present: {map:?}");
    assert!(map.contains_key(&2), "line 2 should be present: {map:?}");

    let line1 = &map[&1];
    assert_eq!(line1.author, "Test Author");
    assert_eq!(line1.sha.len(), 40, "sha should be full hex");
    assert!(line1.timestamp > 0);
}

#[test]
fn blame_for_file_returns_empty_outside_git_repo() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(root.join("loose.txt"), "no git here\n").expect("write");

    let map = blame_for_file(root, Path::new("loose.txt"));
    assert!(map.is_empty());
}

#[test]
fn blame_for_file_cached_round_trips_through_disk_cache() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    make_repo(root);

    // First call: cache miss, runs git.
    let first = blame_for_file_cached(root, Path::new("hello.txt"));
    assert!(!first.is_empty());

    // Second call: cache hit, identical contents.
    let second = blame_for_file_cached(root, Path::new("hello.txt"));
    assert_eq!(first.len(), second.len());
    for (line, info) in &first {
        let other = second.get(line).expect("line missing from cached read");
        assert_eq!(info.sha, other.sha);
        assert_eq!(info.author, other.author);
        assert_eq!(info.timestamp, other.timestamp);
    }
}

#[test]
fn try_blame_from_cache_returns_none_before_first_blame() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    make_repo(root);

    assert!(try_blame_from_cache(root, Path::new("hello.txt")).is_none());
}

#[test]
fn try_blame_from_cache_returns_data_after_first_blame() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    make_repo(root);

    let _ = blame_for_file_cached(root, Path::new("hello.txt"));
    let cached = try_blame_from_cache(root, Path::new("hello.txt"))
        .expect("cache should be populated after blame_for_file_cached");
    assert!(!cached.is_empty());
}

#[test]
fn try_blame_from_cache_invalidates_on_file_change() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    make_repo(root);

    // Warm the cache.
    let _ = blame_for_file_cached(root, Path::new("hello.txt"));
    assert!(try_blame_from_cache(root, Path::new("hello.txt")).is_some());

    // Modify the file's mtime — cache should be considered stale and return
    // None until the next blame call re-populates it.
    let after = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    let file_path = root.join("hello.txt");
    let f = fs::OpenOptions::new()
        .write(true)
        .open(&file_path)
        .expect("open hello");
    f.set_modified(after).expect("set mtime");

    assert!(
        try_blame_from_cache(root, Path::new("hello.txt")).is_none(),
        "cache should be stale after mtime bump"
    );
}

// ---------------------------------------------------------------------------
// blame_for_line_cached — the hot path used by the preview pane
// ---------------------------------------------------------------------------

#[test]
fn blame_for_line_cached_returns_per_line_data() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    make_repo(root);

    let info = blame_for_line_cached(root, Path::new("hello.txt"), 1)
        .expect("line 1 should blame");
    assert_eq!(info.author, "Test Author");
    assert!(!info.sha.is_empty());
    assert!(info.timestamp > 0);
}

#[test]
fn blame_for_line_cached_returns_none_for_untracked_file() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    make_repo(root);
    fs::write(root.join("untracked.txt"), "fresh\n").expect("write");

    assert!(blame_for_line_cached(root, Path::new("untracked.txt"), 1).is_none());
}

#[test]
fn blame_for_line_cached_returns_none_outside_git_repo() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(root.join("loose.txt"), "no git here\n").expect("write");

    assert!(blame_for_line_cached(root, Path::new("loose.txt"), 1).is_none());
}

#[test]
fn blame_for_line_cached_populates_cache_for_subsequent_lookups() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    make_repo(root);

    // First line lookup — cache miss, runs git.
    let first = blame_for_line_cached(root, Path::new("hello.txt"), 1).expect("blame line 1");
    // Now the on-disk cache should contain at least that entry.
    let cached = try_blame_from_cache(root, Path::new("hello.txt"))
        .expect("cache populated after per-line blame");
    let cached_line = cached.get(&1).expect("line 1 in cache");
    assert_eq!(cached_line.sha, first.sha);
    assert_eq!(cached_line.author, first.author);
}

#[test]
fn blame_for_line_cached_merges_into_existing_cache() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    make_repo(root);

    // Populate cache for line 1, then line 2. Both should be retrievable.
    let _ = blame_for_line_cached(root, Path::new("hello.txt"), 1);
    let _ = blame_for_line_cached(root, Path::new("hello.txt"), 2);

    let cached = try_blame_from_cache(root, Path::new("hello.txt"))
        .expect("cache populated");
    assert!(cached.contains_key(&1), "line 1 preserved");
    assert!(cached.contains_key(&2), "line 2 added");
}

// ---------------------------------------------------------------------------
// file_last_touched — the file-level fallback
// ---------------------------------------------------------------------------

#[test]
fn file_last_touched_returns_timestamp_and_author() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    make_repo(root);

    let (ts, author) =
        file_last_touched(root, Path::new("hello.txt")).expect("file has commits");
    assert!(ts > 0);
    assert_eq!(author, "Test Author");
}

#[test]
fn file_last_touched_returns_none_for_untracked_file() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    make_repo(root);
    fs::write(root.join("untracked.txt"), "fresh\n").expect("write");

    assert!(file_last_touched(root, Path::new("untracked.txt")).is_none());
}

#[test]
fn file_last_touched_returns_none_outside_repo() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(root.join("loose.txt"), "no git\n").expect("write");

    assert!(file_last_touched(root, Path::new("loose.txt")).is_none());
}

// ---------------------------------------------------------------------------
// Map accessor helpers — pure functions, useful for the preview formatters
// ---------------------------------------------------------------------------

#[test]
fn line_summary_from_map_formats_short_sha_and_date() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    make_repo(root);

    let map = blame_for_file(root, Path::new("hello.txt"));
    let summary = line_summary_from_map(&map, 1).expect("summary for line 1");
    // Format: "<8-char sha> <YYYY-MM-DD> <author>"
    let mut parts = summary.splitn(3, ' ');
    let sha = parts.next().unwrap();
    let date = parts.next().unwrap();
    let author = parts.next().unwrap();
    assert_eq!(sha.len(), 8, "short sha should be 8 chars: {sha}");
    assert_eq!(date.len(), 10, "ISO date: {date}");
    assert_eq!(author, "Test Author");
}

#[test]
fn line_summary_from_map_returns_none_for_unknown_line() {
    let map = std::collections::HashMap::new();
    assert!(line_summary_from_map(&map, 999).is_none());
}

#[test]
fn latest_change_from_map_picks_newest_timestamp() {
    let _guard = EnvGuard::new();
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    make_repo(root);
    let map = blame_for_file(root, Path::new("hello.txt"));

    let (ts, author) = latest_change_from_map(&map).expect("non-empty map");
    let max_ts = map.values().map(|b| b.timestamp).max().unwrap();
    assert_eq!(ts, max_ts);
    assert_eq!(author, "Test Author");
}

#[test]
fn latest_change_from_map_returns_none_for_empty_map() {
    let map = std::collections::HashMap::new();
    assert!(latest_change_from_map(&map).is_none());
}
