//! End-to-end tests for the headless (`--output`) search mode. Each
//! test builds a small tree in a tempdir, points `YOINK_CONFIG_PATH` at a
//! per-test config so the suite never touches the real `~/.yoink-config`, and
//! runs the compiled binary as a subprocess. Output is asserted on directly
//! (the crate pulls in no JSON parser, so we check structure by substring and
//! line count).

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

/// Run the built binary in `dir` with `args`, using a config file containing
/// `config` (so search_mode / sort / ignore globs are deterministic).
fn run(dir: &Path, config: &str, args: &[&str]) -> Output {
    let config_path = dir.join(".yoink-config");
    fs::write(&config_path, config).expect("write config");
    Command::new(env!("CARGO_BIN_EXE_yoink"))
        .args(args)
        .current_dir(dir)
        .env("YOINK_CONFIG_PATH", &config_path)
        .output()
        .expect("failed to run yoink")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

const REGEX_CONFIG: &str = "search_mode=regex\ncase_sensitive=false\nsort=depth\n.git/**\n";

#[test]
fn json_output_has_envelope_and_results() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("alpha.txt"),
        "one\ntwo needle three\nfour\n",
    )
    .unwrap();

    let output = run(dir.path(), REGEX_CONFIG, &["needle", "-o", "json"]);
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    let body = stdout(&output);

    assert!(body.contains("\"query\": \"needle\""), "body: {body}");
    assert!(body.contains("\"mode\": \"regex\""), "body: {body}");
    assert!(body.contains("\"kind\": \"content\""), "body: {body}");
    assert!(body.contains("\"path\": \"alpha.txt\""), "body: {body}");
    assert!(body.contains("\"line\": 2"), "body: {body}");
    assert!(
        body.contains("\"match\": \"two needle three\""),
        "body: {body}"
    );
    assert!(body.contains("\"count\": 1"), "body: {body}");
}

#[test]
fn context_lines_surround_the_match() {
    let dir = TempDir::new().unwrap();
    // 5 lines; match on line 3, ask for 1 line of context each side.
    fs::write(dir.path().join("file.txt"), "l1\nl2\nl3 needle\nl4\nl5\n").unwrap();

    let output = run(
        dir.path(),
        REGEX_CONFIG,
        &["needle", "-o", "json", "-C", "1"],
    );
    let body = stdout(&output);
    assert!(
        body.contains("\"context_before\": [\"l2\"]"),
        "body: {body}"
    );
    assert!(body.contains("\"context_after\": [\"l4\"]"), "body: {body}");
    assert!(body.contains("\"context_start_line\": 2"), "body: {body}");
}

#[test]
fn jsonl_emits_one_object_per_line() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "needle\nneedle\n").unwrap();
    fs::write(dir.path().join("b.txt"), "needle\n").unwrap();

    let output = run(
        dir.path(),
        REGEX_CONFIG,
        &["needle", "-o", "jsonl", "--content-only"],
    );
    let body = stdout(&output);
    let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 3, "expected 3 occurrences, got: {body}");
    for line in lines {
        assert!(line.starts_with('{') && line.ends_with('}'), "line: {line}");
        assert!(line.contains("\"kind\": \"content\""), "line: {line}");
    }
}

#[test]
fn max_results_truncates_and_flags_it() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("many.txt"),
        "needle\nneedle\nneedle\nneedle\n",
    )
    .unwrap();

    let output = run(
        dir.path(),
        REGEX_CONFIG,
        &[
            "needle",
            "-o",
            "json",
            "--max-results",
            "2",
            "--content-only",
        ],
    );
    let body = stdout(&output);
    assert!(body.contains("\"count\": 2"), "body: {body}");
    assert!(body.contains("\"total_matches\": 4"), "body: {body}");
    assert!(body.contains("\"truncated\": true"), "body: {body}");
}

#[test]
fn markdown_output_has_heading_and_fenced_excerpt() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "before\nneedle here\nafter\n").unwrap();

    let output = run(
        dir.path(),
        REGEX_CONFIG,
        &["needle", "-o", "markdown", "-C", "1", "--content-only"],
    );
    let body = stdout(&output);
    assert!(body.contains("# yoink results: `needle`"), "body: {body}");
    assert!(body.contains("## `a.txt:2:1`"), "body: {body}");
    // The match line is marked with '>' inside a fenced block.
    assert!(body.contains("> needle here"), "body: {body}");
}

#[test]
fn mode_flag_overrides_config() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("dots.txt"), "a.b.c\naxbxc\n").unwrap();

    // Config says regex, but we force glob: `a.b.c` should be treated
    // literally (dots escaped), matching only the "a.b.c" line, not "axbxc".
    let output = run(
        dir.path(),
        REGEX_CONFIG,
        &["a.b.c", "-o", "json", "-m", "glob", "--content-only"],
    );
    let body = stdout(&output);
    assert!(body.contains("\"mode\": \"glob\""), "body: {body}");
    assert!(body.contains("\"count\": 1"), "body: {body}");
    assert!(body.contains("\"match\": \"a.b.c\""), "body: {body}");
}

#[test]
fn json_string_escaping_is_valid() {
    let dir = TempDir::new().unwrap();
    // A line with a quote and a backslash — these must be escaped in JSON.
    fs::write(dir.path().join("q.txt"), "say \"hi\" \\ needle\n").unwrap();

    let output = run(
        dir.path(),
        REGEX_CONFIG,
        &["needle", "-o", "json", "--content-only"],
    );
    let body = stdout(&output);
    assert!(
        body.contains(r#"say \"hi\" \\ needle"#),
        "quote/backslash should be escaped; body: {body}"
    );
}

#[test]
fn empty_query_is_an_error() {
    let dir = TempDir::new().unwrap();
    let output = run(dir.path(), REGEX_CONFIG, &["", "-o", "json"]);
    assert!(!output.status.success(), "empty query should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("non-empty query"), "stderr: {stderr}");
}

#[test]
fn path_name_matches_appear_as_path_kind() {
    let dir = TempDir::new().unwrap();
    // File whose *name* matches but whose contents do not.
    fs::write(dir.path().join("needle_file.rs"), "nothing here\n").unwrap();

    let output = run(dir.path(), REGEX_CONFIG, &["needle", "-o", "jsonl"]);
    let body = stdout(&output);
    assert!(
        body.contains("\"kind\": \"path\""),
        "expected a path-kind match; body: {body}"
    );
    assert!(
        body.contains("\"path\": \"needle_file.rs\""),
        "body: {body}"
    );
}
