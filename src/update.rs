//! Best-effort startup update check.
//!
//! On an interactive launch (and only then), yoink asks GitHub whether a newer
//! release exists — at most once per day, with the answer cached so it never
//! slows a launch more than that. If a newer version is found, it offers to run
//! the same one-line installer the README documents and then restarts itself.
//!
//! Everything here is best-effort: no network, no `curl`, a malformed response,
//! or a declined prompt all just fall through to the normal TUI launch. The
//! whole feature is gated behind the `update_check` config key (default on).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const REPO: &str = "AMitchell-GitHub/yoink";
const RELEASE_API: &str = "https://api.github.com/repos/AMitchell-GitHub/yoink/releases/latest";
const INSTALL_SCRIPT_URL: &str =
    "https://raw.githubusercontent.com/AMitchell-GitHub/yoink/refs/heads/master/scripts/install-from-release.sh";
/// Set on the restarted process so the freshly-installed binary doesn't
/// immediately run the check again.
const SKIP_ENV: &str = "YOINK_SKIP_UPDATE_CHECK";
const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;

const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// Cached result of the most recent check, stored under the user's cache dir so
/// the network is only hit once a day.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    /// Unix seconds of the last successful network check.
    checked_at: i64,
    latest_version: String,
    notes: String,
    /// The latest version the user said "no" to — we don't re-prompt for it.
    declined_version: String,
}

/// Entry point, called from `main` just before the TUI launches. Never errors
/// out the caller; on success it may replace the process (restart) and not
/// return at all.
pub fn run_startup_update_check(enabled: bool) {
    if !enabled || env::var_os(SKIP_ENV).is_some() {
        return;
    }
    // Talk to the controlling terminal directly. yoink's cd-wrapper captures
    // the binary's stdout (the TUI renders to stderr), so gating on stdout — or
    // printing the prompt there — would skip the check / pollute the cd target.
    // Opening /dev/tty sidesteps that and doubles as the "are we interactive?"
    // gate: it won't open under cron, CI, or fully-redirected runs.
    let Some(mut tty) = open_tty() else {
        return;
    };
    if which::which("curl").is_err() {
        return;
    }

    let Some(cache_path) = cache_path() else {
        return;
    };
    let mut cache = read_cache(&cache_path).unwrap_or_default();
    let current = env!("CARGO_PKG_VERSION");
    let now = now_unix();

    let fresh = cache.checked_at != 0
        && now.saturating_sub(cache.checked_at) < CHECK_INTERVAL_SECS
        && !cache.latest_version.is_empty();

    if !fresh {
        match fetch_latest() {
            Some((version, notes)) => {
                cache.latest_version = version;
                cache.notes = notes;
                cache.checked_at = now;
                let _ = write_cache(&cache_path, &cache);
            }
            // Network unavailable: fall back to any cached result, or give up.
            None if cache.latest_version.is_empty() => return,
            None => {}
        }
    }

    let latest = cache.latest_version.clone();
    if latest.is_empty() || !is_newer(&latest, current) || latest == cache.declined_version {
        return;
    }

    match prompt(&mut tty, current, &latest, &cache.notes) {
        Choice::Accepted => {
            let _ = writeln!(tty, "\nyoink: updating…");
            let _ = tty.flush();
            match run_installer() {
                Ok(()) => restart_or_notify(&mut tty),
                Err(error) => {
                    let _ = writeln!(tty, "yoink: update failed: {error}");
                    let _ = writeln!(tty, "yoink: continuing with the current version.");
                }
            }
        }
        Choice::Declined => {
            cache.declined_version = latest;
            let _ = write_cache(&cache_path, &cache);
        }
        Choice::NoInput => {}
    }
}

/// Open the controlling terminal for the update prompt's I/O. `None` (skip the
/// check) when there's no tty — cron, CI, or non-unix.
#[cfg(unix)]
fn open_tty() -> Option<std::fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()
}

#[cfg(not(unix))]
fn open_tty() -> Option<std::fs::File> {
    None
}

enum Choice {
    Accepted,
    Declined,
    NoInput,
}

fn prompt(tty: &mut std::fs::File, current: &str, latest: &str, notes: &str) -> Choice {
    let _ = writeln!(
        tty,
        "\n{BOLD}{CYAN}A new yoink release is available.{RESET}  \
{DIM}(you have v{current}, latest is v{latest}){RESET}"
    );

    let formatted = format_notes(notes);
    if !formatted.is_empty() {
        let _ = writeln!(tty, "\n{BOLD}What's new in v{latest}:{RESET}\n{formatted}");
    }

    let _ = writeln!(
        tty,
        "yoink can update itself now — it takes about 10 seconds and needs almost \
no input."
    );
    let _ = writeln!(
        tty,
        "{DIM}(Disable this check with `update_check = false` in ~/.yoink-config.){RESET}"
    );
    let _ = write!(tty, "{BOLD}Update now?{RESET} [y/N]: ");
    let _ = tty.flush();

    // Read the answer from the same terminal (stdin may be captured/redirected
    // by the cd-wrapper, so don't rely on it).
    let reader_handle = match tty.try_clone() {
        Ok(handle) => handle,
        Err(_) => return Choice::NoInput,
    };
    let mut line = String::new();
    match BufReader::new(reader_handle).read_line(&mut line) {
        Ok(0) | Err(_) => return Choice::NoInput,
        Ok(_) => {}
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Choice::Accepted,
        _ => Choice::Declined,
    }
}

/// Download and run the README installer, inheriting yoink's terminal so the
/// script's own prompts work. The script is fetched to a file first (rather
/// than piped into `bash`) precisely so its `read` prompts read from the
/// terminal instead of consuming the piped script.
fn run_installer() -> Result<()> {
    let script = env::temp_dir().join("yoink-update-install.sh");
    let command = format!(
        "curl -fsSL '{}' -o '{script}' && bash '{script}' '{REPO}'",
        INSTALL_SCRIPT_URL,
        script = script.display(),
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&command);
    // Connect the installer to the terminal directly so its own prompts are
    // visible and aren't swallowed by the cd-wrapper's stdout capture.
    if let (Some(stdin), Some(stdout), Some(stderr)) = (open_tty(), open_tty(), open_tty()) {
        cmd.stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
    }
    let status = cmd.status().context("failed to launch the installer")?;
    let _ = fs::remove_file(&script);
    if !status.success() {
        anyhow::bail!("installer exited with {status}");
    }
    Ok(())
}

/// Replace the current process with the freshly-installed binary so the user
/// lands in the new version immediately. Falls back to a message if exec isn't
/// possible.
fn restart_or_notify(tty: &mut std::fs::File) -> ! {
    let target = installed_binary();
    let args: Vec<String> = env::args().skip(1).collect();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // exec only returns if it failed.
        let error = Command::new(&target).args(&args).env(SKIP_ENV, "1").exec();
        let _ = writeln!(
            tty,
            "yoink: updated, but couldn't restart automatically ({error})."
        );
    }

    #[cfg(not(unix))]
    {
        let _ = (&target, &args);
    }

    let _ = writeln!(
        tty,
        "yoink updated — run `yoink` again to use the new version."
    );
    std::process::exit(0);
}

/// Best guess at the installed binary path: the release installer drops it in
/// `~/.local/bin`, so prefer that; otherwise resolve via PATH, then fall back
/// to the current executable (which is the new binary if it was overwritten in
/// place).
fn installed_binary() -> PathBuf {
    if let Some(home) = env::var_os("HOME") {
        let candidate = PathBuf::from(home).join(".local/bin/yoink");
        if candidate.exists() {
            return candidate;
        }
    }
    if let Ok(path) = which::which("yoink") {
        return path;
    }
    env::current_exe().unwrap_or_else(|_| PathBuf::from("yoink"))
}

fn fetch_latest() -> Option<(String, String)> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "3",
            "--connect-timeout",
            "3",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: yoink-update-check",
            RELEASE_API,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let tag = json.get("tag_name")?.as_str()?.trim().to_string();
    let notes = json
        .get("body")
        .and_then(|body| body.as_str())
        .unwrap_or("")
        .to_string();
    let version = tag.trim_start_matches('v').to_string();
    if version.is_empty() {
        return None;
    }
    Some((version, notes))
}

fn cache_path() -> Option<PathBuf> {
    let base = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(base.join("yoink").join("update-check.json"))
}

fn read_cache(path: &Path) -> Option<Cache> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn write_cache(path: &Path, cache: &Cache) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(cache)?)?;
    Ok(())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Compare dotted version strings numerically (`v` prefix and any pre-release
/// suffix on a component are ignored). Returns true when `latest` > `current`.
fn is_newer(latest: &str, current: &str) -> bool {
    let a = parse_version(latest);
    let b = parse_version(current);
    for index in 0..a.len().max(b.len()) {
        let x = a.get(index).copied().unwrap_or(0);
        let y = b.get(index).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

fn parse_version(value: &str) -> Vec<u64> {
    value
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u64>().unwrap_or(0)
        })
        .collect()
}

/// Remove Markdown `![alt](url)` images and HTML `<img …>` tags from release
/// notes; the prompt is text-only.
fn strip_images(notes: &str) -> String {
    let no_md = regex::Regex::new(r"!\[[^\]]*\]\([^)]*\)")
        .map(|re| re.replace_all(notes, "").into_owned())
        .unwrap_or_else(|_| notes.to_string());
    regex::Regex::new(r"(?is)<img[^>]*>")
        .map(|re| re.replace_all(&no_md, "").into_owned())
        .unwrap_or(no_md)
        .trim()
        .to_string()
}

/// Strip images, then indent and cap the notes so a long changelog can't take
/// over the screen before the prompt.
fn format_notes(notes: &str) -> String {
    const MAX_LINES: usize = 25;
    let stripped = strip_images(notes);
    let mut out = String::new();
    for (index, line) in stripped.lines().enumerate() {
        if index >= MAX_LINES {
            out.push_str("  …\n");
            break;
        }
        out.push_str("  ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions_detected() {
        assert!(is_newer("3.2.1", "3.2.0"));
        assert!(is_newer("3.3.0", "3.2.9"));
        assert!(is_newer("4.0.0", "3.9.9"));
        assert!(is_newer("v3.2.0", "3.1.9")); // tolerates the v prefix
        assert!(is_newer("3.2", "3.1.5")); // uneven component counts
    }

    #[test]
    fn equal_or_older_not_newer() {
        assert!(!is_newer("3.2.0", "3.2.0"));
        assert!(!is_newer("3.2.0", "v3.2.0"));
        assert!(!is_newer("3.1.9", "3.2.0"));
        assert!(!is_newer("3.2.0", "3.2.0.1"));
    }

    #[test]
    fn images_are_stripped() {
        let notes = "Intro\n![banner](https://example.com/a.png)\nMiddle\n<img src=\"x.gif\">\nEnd";
        let stripped = strip_images(notes);
        assert!(!stripped.contains("!["), "md image left: {stripped}");
        assert!(
            !stripped.to_lowercase().contains("<img"),
            "html left: {stripped}"
        );
        assert!(stripped.contains("Intro") && stripped.contains("End"));
    }

    #[test]
    fn notes_are_capped() {
        let many = (0..100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let formatted = format_notes(&many);
        assert!(formatted.contains('…'), "should be truncated: {formatted}");
        assert!(formatted.lines().count() <= 27);
    }
}
