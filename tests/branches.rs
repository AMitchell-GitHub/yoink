//! End-to-end tests for cross-branch search (`--branches` / `--ref`). Each test
//! builds a throwaway git repo with commits on multiple branches, then runs the
//! compiled binary as a subprocess. `--no-fetch` keeps the runs offline/fast,
//! and a per-test `YOINK_CONFIG_PATH` never touches the real `~/.yoink-config`.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

/// Run a git command in `dir` with a fixed identity and no global/system config
/// interference. Panics on failure.
fn git(dir: &Path, args: &[&str]) {
    let output = git_cmd(dir).args(args).output().expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_cmd(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "tester@example.com")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "tester@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    cmd
}

/// Stage everything and commit, dating the commit at `date` (any format git
/// accepts) so the `--since` timeframe filter is testable.
fn commit(dir: &Path, date: &str) {
    git(dir, &["add", "-A"]);
    let output = git_cmd(dir)
        .args(["commit", "-q", "-m", "c"])
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .expect("spawn git commit");
    assert!(
        output.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(dir: &Path, args: &[&str]) -> String {
    let output = git_cmd(dir).args(args).output().expect("spawn git");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn yoink(dir: &Path, args: &[&str]) -> Output {
    let config_path = dir.join(".yoink-config");
    fs::write(
        &config_path,
        "search_mode=glob\ncase_sensitive=false\nsort=depth\n.git/**\n",
    )
    .expect("write config");
    Command::new(env!("CARGO_BIN_EXE_yoink"))
        .args(args)
        .current_dir(dir)
        .env("YOINK_CONFIG_PATH", &config_path)
        .output()
        .expect("run yoink")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repo on branch `main` with one commit and no interesting term.
fn setup_repo(dir: &Path) {
    git(dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
    fs::write(dir.join("readme.md"), "hello world\n").unwrap();
    commit(dir, "2026-01-01T00:00:00");
}

/// Add `content` to `readme.md` on a fresh branch, dated `date`, then return to
/// `main`.
fn branch_with(dir: &Path, branch: &str, content: &str, date: &str) {
    git(dir, &["checkout", "-q", "-b", branch]);
    fs::write(dir.join("readme.md"), content).unwrap();
    commit(dir, date);
    git(dir, &["checkout", "-q", "main"]);
}

#[test]
fn finds_term_on_other_branch_but_not_current() {
    let dir = TempDir::new().unwrap();
    setup_repo(dir.path());
    branch_with(
        dir.path(),
        "feature/x",
        "hello world\nSECRET_TOKEN_XYZ\n",
        "2026-06-01T00:00:00",
    );

    // From `main`, the term isn't in the working tree, but branch search finds it.
    let output = yoink(
        dir.path(),
        &[
            "SECRET_TOKEN_XYZ",
            "--branches",
            "--no-fetch",
            "-o",
            "jsonl",
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body = stdout(&output);
    assert!(body.contains("\"branch\":\"feature/x\""), "body: {body}");
    assert!(body.contains("\"line\":2"), "body: {body}");
    assert!(body.contains("SECRET_TOKEN_XYZ"), "body: {body}");
    // `main` never carried the term.
    assert!(!body.contains("\"branch\":\"main\""), "body: {body}");
}

#[test]
fn branch_filter_limits_refs() {
    let dir = TempDir::new().unwrap();
    setup_repo(dir.path());
    branch_with(
        dir.path(),
        "feat/keep",
        "x\nNEEDLE\n",
        "2026-06-01T00:00:00",
    );
    branch_with(
        dir.path(),
        "other/skip",
        "x\nNEEDLE\n",
        "2026-06-01T00:00:00",
    );

    let output = yoink(
        dir.path(),
        &[
            "NEEDLE",
            "--branches",
            "--no-fetch",
            "--branch-filter",
            "feat/*",
            "-o",
            "jsonl",
        ],
    );
    let body = stdout(&output);
    assert!(body.contains("\"branch\":\"feat/keep\""), "body: {body}");
    assert!(!body.contains("other/skip"), "body: {body}");
}

#[test]
fn since_excludes_old_branches() {
    let dir = TempDir::new().unwrap();
    setup_repo(dir.path());
    // Ancient branch: never within any reasonable window.
    branch_with(
        dir.path(),
        "old/feature",
        "x\nNEEDLE\n",
        "2000-01-01T00:00:00",
    );
    // Fresh branch: committed "now" (no date override), always within 30d.
    git(dir.path(), &["checkout", "-q", "-b", "new/feature"]);
    fs::write(dir.path().join("readme.md"), "x\nNEEDLE\n").unwrap();
    git(dir.path(), &["add", "-A"]);
    let out = git_cmd(dir.path())
        .args(["commit", "-q", "-m", "c"])
        .output()
        .unwrap();
    assert!(out.status.success());
    git(dir.path(), &["checkout", "-q", "main"]);

    let output = yoink(
        dir.path(),
        &[
            "NEEDLE",
            "--branches",
            "--no-fetch",
            "--since",
            "30d",
            "-o",
            "jsonl",
        ],
    );
    let body = stdout(&output);
    assert!(body.contains("\"branch\":\"new/feature\""), "body: {body}");
    assert!(!body.contains("old/feature"), "body: {body}");
}

#[test]
fn ref_searches_a_specific_commit() {
    let dir = TempDir::new().unwrap();
    setup_repo(dir.path());
    branch_with(
        dir.path(),
        "feature/x",
        "x\nNEEDLE\n",
        "2026-06-01T00:00:00",
    );
    let short = git_stdout(dir.path(), &["rev-parse", "--short", "feature/x"]);
    assert!(!short.is_empty());

    let output = yoink(
        dir.path(),
        &["NEEDLE", "--ref", &short, "--no-fetch", "-o", "jsonl"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body = stdout(&output);
    assert!(
        body.contains(&format!("\"branch\":\"{short}\"")),
        "body: {body}"
    );
    assert!(body.contains("NEEDLE"), "body: {body}");
}

#[test]
fn max_results_stops_early() {
    let dir = TempDir::new().unwrap();
    setup_repo(dir.path());
    // Two branches, each with the term — without a cap we'd get 2+ hits.
    branch_with(dir.path(), "a/one", "x\nNEEDLE\n", "2026-06-02T00:00:00");
    branch_with(dir.path(), "b/two", "x\nNEEDLE\n", "2026-06-01T00:00:00");

    let output = yoink(
        dir.path(),
        &[
            "NEEDLE",
            "--branches",
            "--no-fetch",
            "--max-results",
            "1",
            "-o",
            "jsonl",
        ],
    );
    let body = stdout(&output);
    let lines = body.lines().filter(|l| l.contains("\"branch\"")).count();
    assert_eq!(lines, 1, "expected exactly one hit, got: {body}");
}

#[test]
fn errors_cleanly_outside_a_repo() {
    let dir = TempDir::new().unwrap();
    // No `git init` — not a repo.
    fs::write(dir.path().join("f.txt"), "NEEDLE\n").unwrap();
    let output = yoink(dir.path(), &["NEEDLE", "--branches", "--no-fetch"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not inside a git repository"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
