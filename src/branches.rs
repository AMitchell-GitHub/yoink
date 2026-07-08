//! Cross-branch search: find a term in git refs (branches / tags / commits)
//! without checking anything out.
//!
//! yoink already shells out to `git` for blame (see `blame.rs`); this does the
//! same with two read-only plumbing commands:
//!   * `git for-each-ref --sort=-committerdate` enumerates refs newest-first
//!     with their committer date (for the timeframe filter), and
//!   * `git grep <ref>` searches a ref's tree *in place*.
//!
//! Nothing touches the working tree or `HEAD`, so it is safe to run with local
//! edits in flight and is far faster than checking each branch out (~30 ms per
//! ref, measured, versus a full working-tree write per checkout).
//!
//! The engine is transport-agnostic: `search_branches` drives the search and
//! hands every progress/result event to a caller-supplied sink, so the headless
//! CLI (stream to stdout) and the TUI (send over its event channel) share one
//! implementation. Refs are searched serially newest-first so early-exit
//! (`max_results`) is deterministic — the first hits are always the newest
//! branches.

use crate::search::{effective_pattern, SearchMode};
use anyhow::{bail, Context, Result};
use regex::Regex;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A ref to search, resolved from the user's filter/target. `token` is the
/// exact tree-ish handed to `git grep` and echoed back in results (a short
/// refname like `origin/jb/feat/…`, or a commit hash the user typed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefTarget {
    pub token: String,
    /// Committer date (unix). `None` for a bare commit target we didn't date.
    pub committed_ts: Option<i64>,
    pub is_remote: bool,
}

/// One content match on one ref.
#[derive(Debug, Clone)]
pub struct BranchHit {
    pub reference: String,
    pub committed_ts: Option<i64>,
    pub path: PathBuf,
    pub line: usize,
    pub content: String,
}

/// Everything the engine needs. `mode`/`case_sensitive` come from the resolved
/// settings so glob/regex + case behave exactly like the working-tree search.
pub struct BranchSearchOptions {
    pub query: String,
    pub mode: SearchMode,
    pub case_sensitive: bool,
    /// Branch-name glob (e.g. `jb/*`) OR a commit-ish typed into the target
    /// field. Empty/`None` = every ref.
    pub filter: Option<String>,
    /// Explicit refs/commits from `--ref`; when non-empty, enumeration and the
    /// name/timeframe filters are skipped and exactly these are searched.
    pub explicit_refs: Vec<String>,
    /// Only refs updated within this window are searched. `None` = no limit.
    pub since: Option<Duration>,
    /// Which ref namespaces to enumerate: local heads and/or remote-tracking.
    pub include_local: bool,
    pub include_remotes: bool,
    pub fetch: bool,
    /// Stop after this many hits (early-exit). `None` = no cap.
    pub max_results: Option<usize>,
}

/// Progress + result events, emitted in order as the search runs.
pub enum BranchEvent {
    /// A `git fetch` is starting (only when `fetch` is set).
    Fetching,
    /// The fetch failed; the search continues with existing refs.
    FetchFailed(String),
    /// Ref set resolved; `total` refs will be searched.
    Enumerated { total: usize },
    /// About to search ref `index` (0-based) of `total`.
    RefStarted {
        index: usize,
        total: usize,
        name: String,
    },
    /// A content match.
    Hit(BranchHit),
    /// The run is done (or was capped/cancelled).
    Finished {
        searched: usize,
        hits: usize,
        truncated: bool,
    },
}

/// Drive a cross-branch search, handing every event to `sink`. Checks `cancel`
/// between refs (and while reading a ref's matches) so the caller can stop a
/// long run. Errors only for setup failures (e.g. `git for-each-ref` failing);
/// a single ref that fails to grep is skipped, not fatal.
pub fn search_branches<F>(
    repo_root: &Path,
    opts: &BranchSearchOptions,
    cancel: &AtomicBool,
    mut sink: F,
) -> Result<()>
where
    F: FnMut(BranchEvent),
{
    // Only fetch when remote-tracking refs are actually in scope — no point
    // hitting the network to search local heads.
    if opts.fetch && opts.include_remotes && !cancel.load(Ordering::Relaxed) {
        sink(BranchEvent::Fetching);
        if let Err(error) = run_fetch(repo_root) {
            sink(BranchEvent::FetchFailed(error.to_string()));
        }
    }

    let targets = resolve_targets(repo_root, opts)?;
    let total = targets.len();
    sink(BranchEvent::Enumerated { total });

    // Reuse the working-tree pattern pipeline so glob/regex semantics match
    // exactly; pass `case_sensitive = true` to keep the raw pattern free of the
    // `(?i)` inline flag (git's basic/extended regex would treat it literally),
    // then let git handle case via `-i`.
    let raw = effective_pattern(&opts.query, opts.mode, true)?;
    if raw.is_empty() || total == 0 {
        sink(BranchEvent::Finished {
            searched: 0,
            hits: 0,
            truncated: false,
        });
        return Ok(());
    }

    let use_pcre = detect_pcre(repo_root);
    let case_insensitive = !opts.case_sensitive;

    let mut hits = 0usize;
    let mut searched = 0usize;
    let mut truncated = false;

    'refs: for (index, target) in targets.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        sink(BranchEvent::RefStarted {
            index,
            total,
            name: target.token.clone(),
        });
        let matches = grep_ref(
            repo_root,
            &target.token,
            &raw,
            case_insensitive,
            use_pcre,
            cancel,
        );
        searched += 1;
        for (path, line, content) in matches {
            sink(BranchEvent::Hit(BranchHit {
                reference: target.token.clone(),
                committed_ts: target.committed_ts,
                path,
                line,
                content,
            }));
            hits += 1;
            if let Some(max) = opts.max_results {
                if hits >= max {
                    // We stopped early; unsearched refs may hold more hits.
                    truncated = index + 1 < total;
                    break 'refs;
                }
            }
        }
    }

    sink(BranchEvent::Finished {
        searched,
        hits,
        truncated,
    });
    Ok(())
}

/// Resolve the set of refs to search from the options, newest-first.
fn resolve_targets(repo_root: &Path, opts: &BranchSearchOptions) -> Result<Vec<RefTarget>> {
    // Explicit `--ref` targets win; keep only those git can resolve to a commit.
    if !opts.explicit_refs.is_empty() {
        let mut out = Vec::new();
        for token in &opts.explicit_refs {
            if resolve_commit(repo_root, token) {
                out.push(RefTarget {
                    token: token.clone(),
                    committed_ts: commit_timestamp(repo_root, token),
                    is_remote: false,
                });
            }
        }
        return Ok(out);
    }

    // A target that looks like a commit hash and resolves is searched directly.
    if let Some(filter) = opts.filter.as_deref() {
        let trimmed = filter.trim();
        if !trimmed.is_empty() && looks_like_commit(trimmed) && resolve_commit(repo_root, trimmed) {
            return Ok(vec![RefTarget {
                token: trimmed.to_string(),
                committed_ts: commit_timestamp(repo_root, trimmed),
                is_remote: false,
            }]);
        }
    }

    // Otherwise enumerate refs and apply the name-glob + timeframe filters.
    let mut refs = enumerate_refs(repo_root, opts.include_local, opts.include_remotes)?;

    if let Some(filter) = opts.filter.as_deref() {
        let trimmed = filter.trim();
        if !trimmed.is_empty() {
            let matcher = branch_filter_regex(trimmed)?;
            refs.retain(|candidate| matcher.is_match(&candidate.token));
        }
    }

    if let Some(window) = opts.since {
        let cutoff = now_unix().saturating_sub(window.as_secs() as i64);
        refs.retain(|candidate| {
            candidate
                .committed_ts
                .map(|ts| ts >= cutoff)
                .unwrap_or(true)
        });
    }

    Ok(refs)
}

/// Enumerate local heads and/or remote-tracking refs, newest-first, with
/// committer dates. Symbolic refs (e.g. `origin/HEAD`) are skipped. With
/// neither namespace requested, returns an empty list without running git.
fn enumerate_refs(
    repo_root: &Path,
    include_local: bool,
    include_remotes: bool,
) -> Result<Vec<RefTarget>> {
    if !include_local && !include_remotes {
        return Ok(Vec::new());
    }
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_root)
        .arg("for-each-ref")
        .arg("--sort=-committerdate")
        .arg("--format=%(refname)%09%(refname:short)%09%(committerdate:unix)%09%(symref)");
    if include_local {
        cmd.arg("refs/heads");
    }
    if include_remotes {
        cmd.arg("refs/remotes");
    }

    let output = cmd
        .output()
        .context("failed to run git for-each-ref (is this a git repository?)")?;
    if !output.status.success() {
        bail!("git for-each-ref failed: {}", output.status);
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut refs = Vec::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let full = fields.next().unwrap_or("");
        let short = fields.next().unwrap_or("");
        let ts = fields.next().unwrap_or("");
        let symref = fields.next().unwrap_or("");
        // Skip symbolic refs (origin/HEAD) so we don't search the same commit
        // twice under a misleading name.
        if !symref.is_empty() || full.ends_with("/HEAD") || short.is_empty() {
            continue;
        }
        refs.push(RefTarget {
            token: short.to_string(),
            committed_ts: ts.parse::<i64>().ok(),
            is_remote: full.starts_with("refs/remotes/"),
        });
    }
    Ok(refs)
}

/// Run `git grep` for `pattern` at `token`, returning `(path, line, content)`
/// per match. Returns empty on any error (a bad ref, a pattern the engine
/// rejects) rather than failing the whole search. Honors `cancel` between
/// lines and kills the child if asked to stop.
fn grep_ref(
    repo_root: &Path,
    token: &str,
    pattern: &str,
    case_insensitive: bool,
    use_pcre: bool,
    cancel: &AtomicBool,
) -> Vec<(PathBuf, usize, String)> {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_root)
        .arg("grep")
        .arg("-n")
        .arg("-I")
        .arg("--no-color");
    if case_insensitive {
        cmd.arg("-i");
    }
    cmd.arg(if use_pcre { "-P" } else { "-E" });
    cmd.arg("-e").arg(pattern).arg(token);

    let mut child = match cmd.stdout(Stdio::piped()).stderr(Stdio::null()).spawn() {
        Ok(child) => child,
        Err(_) => return Vec::new(),
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.wait();
        return Vec::new();
    };

    // `git grep <ref>` prefixes each line with the exact tree-ish token we
    // passed, followed by ':'. Strip that, then split off path:line:content.
    let prefix = format!("{token}:");
    let reader = BufReader::new(stdout);
    let mut result = Vec::new();
    for raw in reader.lines().map_while(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            break;
        }
        let Some(rest) = raw.strip_prefix(&prefix) else {
            continue;
        };
        let mut parts = rest.splitn(3, ':');
        let path = parts.next().unwrap_or("");
        let Some(line_str) = parts.next() else {
            continue;
        };
        let content = parts.next().unwrap_or("");
        let Ok(line) = line_str.parse::<usize>() else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        result.push((
            PathBuf::from(path),
            line,
            content.replace('\t', " ").trim().to_string(),
        ));
    }
    let _ = child.wait();
    result
}

/// Probe whether this git supports PCRE (`grep -P`). Runs a `-q` (first-match,
/// short-circuiting) grep against `HEAD`; exit code < 2 means it works. Falls
/// back to extended regex (`-E`) when PCRE is unavailable or HEAD is unborn.
fn detect_pcre(repo_root: &Path) -> bool {
    Command::new("git")
        .current_dir(repo_root)
        .args(["grep", "-qP", "-e", "a", "HEAD"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()
        .and_then(|status| status.code())
        .map(|code| code < 2)
        .unwrap_or(false)
}

/// Best-effort `git fetch --all`. `GIT_TERMINAL_PROMPT=0` makes auth failures
/// fail fast instead of blocking on a credential prompt.
fn run_fetch(repo_root: &Path) -> Result<()> {
    let status = Command::new("git")
        .current_dir(repo_root)
        .args(["fetch", "--all", "--quiet"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to spawn git fetch")?;
    if status.success() {
        Ok(())
    } else {
        bail!("git fetch exited with {status}");
    }
}

/// True if `git rev-parse` resolves `spec` to a commit.
fn resolve_commit(repo_root: &Path, spec: &str) -> bool {
    Command::new("git")
        .current_dir(repo_root)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{spec}^{{commit}}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Committer date (unix) of a commit-ish, if resolvable.
fn commit_timestamp(repo_root: &Path, spec: &str) -> Option<i64> {
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(["show", "-s", "--format=%ct"])
        .arg(spec)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Short-refnames of local + remote branches that contain `commit`. Used as a
/// bonus annotation when searching a bare commit hash.
pub fn branches_containing(repo_root: &Path, commit: &str) -> Vec<String> {
    let output = match Command::new("git")
        .current_dir(repo_root)
        .args(["branch", "-a", "--contains"])
        .arg(commit)
        .arg("--format=%(refname:short)")
        .stderr(Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.ends_with("/HEAD"))
        .map(str::to_string)
        .collect()
}

/// A commit-ish typed into the target field: 4–40 hex chars, no glob
/// metacharacters. Callers still confirm it resolves via `resolve_commit`.
fn looks_like_commit(spec: &str) -> bool {
    let len = spec.len();
    (4..=40).contains(&len) && spec.chars().all(|c| c.is_ascii_hexdigit())
}

/// Translate a branch-name glob into a case-insensitive substring regex. Unlike
/// the path-aware glob matcher used for file search, `*` here spans `/` so a
/// filter like `jb/*` matches remote-tracking branches such as
/// `origin/jb/feat/…` (matched as a substring of the short refname).
fn branch_filter_regex(filter: &str) -> Result<Regex> {
    let mut pattern = String::from("(?i)");
    for ch in filter.chars() {
        match ch {
            '*' => pattern.push_str(".*"),
            '?' => pattern.push('.'),
            other => pattern.push_str(&regex::escape(&other.to_string())),
        }
    }
    Regex::new(&pattern).with_context(|| format!("invalid branch filter: {filter}"))
}

/// Parse a timeframe spec into a duration window. Accepts a bare number (days)
/// or a number with a unit: `h`(ours) `d`(ays) `w`(eeks) `mo`(nths, =30d)
/// `y`(ears), plus `m`(inutes). Empty / `all` / `any` mean no limit (`None`).
pub fn parse_timeframe(spec: &str) -> Result<Option<Duration>> {
    let normalized = spec.trim().to_ascii_lowercase();
    if normalized.is_empty() || normalized == "all" || normalized == "any" {
        return Ok(None);
    }
    let split = normalized
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(normalized.len());
    let (number, unit) = normalized.split_at(split);
    let count: u64 = number
        .parse()
        .with_context(|| format!("invalid timeframe: {spec}"))?;
    let seconds = match unit {
        "" | "d" | "day" | "days" => count * 86_400,
        "h" | "hour" | "hours" => count * 3_600,
        "m" | "min" | "mins" | "minute" | "minutes" => count * 60,
        "w" | "week" | "weeks" => count * 604_800,
        "mo" | "month" | "months" => count * 2_592_000,
        "y" | "year" | "years" => count * 31_536_000,
        other => bail!("unknown timeframe unit '{other}' (use d, w, mo, y, h, m, or 'all')"),
    };
    Ok(Some(Duration::from_secs(seconds)))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeframe_units() {
        assert_eq!(parse_timeframe("").unwrap(), None);
        assert_eq!(parse_timeframe("all").unwrap(), None);
        assert_eq!(
            parse_timeframe("30d").unwrap(),
            Some(Duration::from_secs(30 * 86_400))
        );
        assert_eq!(
            parse_timeframe("2w").unwrap(),
            Some(Duration::from_secs(2 * 604_800))
        );
        assert_eq!(
            parse_timeframe("3mo").unwrap(),
            Some(Duration::from_secs(3 * 2_592_000))
        );
        assert_eq!(
            parse_timeframe("1y").unwrap(),
            Some(Duration::from_secs(31_536_000))
        );
        // Bare number defaults to days.
        assert_eq!(
            parse_timeframe("7").unwrap(),
            Some(Duration::from_secs(7 * 86_400))
        );
        // Minutes vs months disambiguation.
        assert_eq!(
            parse_timeframe("5m").unwrap(),
            Some(Duration::from_secs(300))
        );
        assert!(parse_timeframe("bogus").is_err());
        assert!(parse_timeframe("10q").is_err());
    }

    #[test]
    fn commit_shape() {
        assert!(looks_like_commit("1b7c2113"));
        assert!(looks_like_commit(
            "1b7c21138b8497aa33db3ee0e34dd69edc7526f1"
        ));
        assert!(!looks_like_commit("jb/*"));
        assert!(!looks_like_commit("abc")); // too short
        assert!(!looks_like_commit("master"));
    }

    #[test]
    fn branch_filter_matches_substring_across_slash() {
        let re = branch_filter_regex("jb/*").unwrap();
        assert!(re.is_match("origin/jb/feat/260630-operator-presence-redesign"));
        assert!(re.is_match("jb/quick"));
        assert!(!re.is_match("origin/master"));
        // Case-insensitive.
        assert!(branch_filter_regex("JB/*").unwrap().is_match("origin/jb/x"));
    }
}
