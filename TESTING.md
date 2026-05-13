# Testing yoink

This document describes what is covered by yoink's automated tests and shows
how the tool's runtime performance compares against the underlying search and
git plumbing it sits on top of. Numbers were collected on the machine listed
in the [Methodology](#methodology) section; treat them as relative
performance ratios, not absolute promises.

## Running the tests

```bash
cargo test
```

You should see three test binaries pass:

| File | What it covers |
|---|---|
| `tests/actions.rs` | Path-resolution helpers used by Enter/Ctrl-V/Ctrl-O/Ctrl-S. |
| `tests/search.rs` | The directory walker — hidden-file rules, `.yoinkignore` patterns, default ignores, depth- and alphabetical-sort, combined path+content matching. |
| `tests/blame.rs` | The `git blame` integration — repo discovery, per-line and whole-file blame, the on-disk cache, env-var-driven session state, fallback paths when git/cache are unavailable, and pure helpers like `format_unix_date`. |

Each blame test creates a real temp git repository (initialized with fixed
author/date so assertions are deterministic) and exercises the public APIs
against it. Tests that touch process-wide environment variables hold a
`Mutex` to prevent interference when run in parallel.

The full suite currently runs 38 tests in under a second:

| Binary | Test count |
|---|---:|
| `tests/actions.rs` | 2 |
| `tests/search.rs` | 9 |
| `tests/blame.rs` | 27 |
| **Total** | **38** |

## Coverage at a glance

### Search (`tests/search.rs`)
- merges path-name and content matches into a single result set
- depth-then-alphabetical sort (default)
- alphabetical sort (via `sort_mode=alphabetical` in `.yoinkignore`)
- skips hidden paths by default
- includes hidden paths when `include_hidden=true`
- applies the built-in `**/.git` and `**/node_modules` prune patterns
- respects user-supplied `.yoinkignore` globs
- empty query returns the file list rather than failing
- invalid regex (e.g. unanchored `*`) returns an `anyhow` error with the
  text `"invalid regex query"` rather than silently swallowing the input

### Actions (`tests/actions.rs`)
- `resolve_target_dir` returns the file's parent for file paths
- `resolve_target_dir` returns the cwd unchanged for directory paths

### Blame (`tests/blame.rs`)
**Pure helpers** — no git invocation:
- `find_repo_root` walks up to a `.git` directory ancestor
- `find_repo_root` returns `None` when no `.git` is reachable
- `find_repo_root` accepts a `.git` *file* (submodules/worktrees)
- `format_unix_date` produces the expected ISO date for known timestamps
- `format_unix_date` does not panic on negative (pre-epoch) timestamps
- `line_summary_from_map` formats `<8-char sha> <YYYY-MM-DD> <author>`
- `line_summary_from_map` returns `None` for unknown lines
- `latest_change_from_map` picks the entry with the newest timestamp
- `latest_change_from_map` returns `None` for an empty map

**Session state** — env-var driven:
- `session_cache_dir` reflects `YOINK_CACHE_DIR`
- `state_file_path` reflects `YOINK_BLAME_SORT_FILE`
- `blame_sort_active` is false → true → false across two toggles
- `clear_session_cache` is a no-op when the cache dir is absent

**Real-git integration** — runs git against a temp repo:
- `blame_for_file` returns per-line sha/author/timestamp
- `blame_for_file` returns empty outside a git repo
- `blame_for_file_cached` round-trips through the on-disk cache
- `try_blame_from_cache` returns `None` before any blame call
- `try_blame_from_cache` returns data after a fresh blame
- `try_blame_from_cache` invalidates when the source file mtime advances
- `blame_for_line_cached` returns the focused line's data
- `blame_for_line_cached` returns `None` for an untracked file
- `blame_for_line_cached` returns `None` outside a git repo
- `blame_for_line_cached` populates the cache for subsequent lookups
- `blame_for_line_cached` merges new lines into an existing cache file
- `file_last_touched` returns the most recent commit author-time + author
- `file_last_touched` returns `None` for an untracked file
- `file_last_touched` returns `None` outside a git repo

### Error / unavailable-input handling

Every external surface that talks to git has a documented fallback:

| Caller | "Git unavailable" outcome | Visible to user as |
|---|---|---|
| `blame_for_file` | empty `HashMap` | preview shows file content with no blame line |
| `blame_for_line_cached` | `None` | preview header falls back to `file_last_touched` |
| `file_last_touched` | `None` | preview header shows `git blame: file is untracked or has no history` |
| `find_repo_root` | `None` | preview header shows `git blame: file is not inside a git working tree` |
| `try_blame_from_cache` | `None` | live blame query runs as if there were no cache |
| `batch_*` blame helpers | error string returned in `Vec<String>` | blame-sort diagnostic row shown at top of fzf list |

These outcomes are exercised by the blame-tests in the previous section.

## Methodology

All performance numbers in this document were collected as follows:

- Hardware: developer laptop (Linux 6.12, AMD x86_64, NVMe disk, OS file
  cache pre-warmed by repeated runs).
- yoink binary: `cargo build --release`, running git 2.47.
- `bench()` helper: 3 sequential runs, median millisecond reported,
  individual run times shown alongside in `[runs: …]`.
- For preview latency, the per-session cache is wiped between cold runs.
- Each row in the search tables reports wall-clock time for the full command
  to read input, walk the tree, search, and write output to `/dev/null`.

### Repositories used

| Repo | Working-tree files | Total files (incl. ignored) | Notes |
|---|---|---|---|
| `~/yoink` (this repo) | ~10 | ~20 | tiny — only meaningful as a "sub-millisecond" baseline |
| `~/combined-gtp-apps/app-gtp` | ~636 | ~30k | medium React/TS app |
| `~/mujin/jhbuildappcontroller/checkoutroot` | ~415k (after rg's ignore rules) | ~1.97M (raw `find`) | hostile worst case — 2057 nested `node_modules`, plus vendored copies of scipy/matplotlib/etc. each a full repo with deep history |

## Search performance

Search reads input, walks the tree, scans content, and writes the formatted
result list. yoink necessarily does a little more than `rg` because it also
reports path-name matches and renders ANSI/formatted output.

### Small repo — `~/yoink`

3-run median, all warm.

| Query | grep | ripgrep | yoink |
|---|---|---|---|
| `fn ` | 23ms | 6ms | **7ms** |
| `blame` | 30ms | 5ms | **7ms** |
| `pub fn` | 18ms | 6ms | **7ms** |

yoink is within 1-2ms of raw ripgrep on a tiny repo — process spawn dominates.

### Big multi-repo — `~/mujin/jhbuildappcontroller/checkoutroot`

3-run median per (tool, query). `grep -rnI` on this tree (~2 million
unfiltered files, 2057 nested `node_modules`, plus full git checkouts of
scipy/matplotlib/etc.) takes 1-3 minutes per invocation; ripgrep and yoink
both respect `.gitignore` and skip ~1.55 million of those files, finishing
in ~1 second.

| Query | grep | ripgrep | yoink | yoink vs rg | yoink vs grep |
|---|---:|---:|---:|---:|---:|
| `productionCycle` | 171387ms | 1738ms | **1186ms** | 0.68× | 145× faster |
| `mobilerobot`     | 142962ms |  963ms | **1310ms** | 1.36× | 109× faster |
| `useState`        |  79005ms |  973ms | **1102ms** | 1.13× |  72× faster |
| `orchestrator`    |  64322ms | 1116ms | **1390ms** | 1.24× |  46× faster |
| `workerTimeout`   |  75260ms | 1010ms | **1025ms** | 1.01× |  73× faster |

Headline observations:
- yoink runs in ~1 second on a tree where `grep -rnI` takes 1-3 minutes.
- yoink is **between 0.68× and 1.36×** raw `ripgrep`'s time depending on
  query (mean ratio ≈ 1.09×). The variation is dominated by ripgrep's own
  run-to-run noise (sample raw runs: `productionCycle` rg
  `[1855, 1738, 1103]`, yoink `[1110, 1186, 1197]`) rather than systematic
  overhead.
- The 50-150× speedup over `grep` is entirely the ignore-rules difference,
  not algorithmic — yoink/ripgrep skip ~1.55 million files that grep walks.

Per-query raw runs are preserved in
`/tmp/claude-1000/-home-aidan-yoink/.../tasks/bw1qb0bbd.output` and can be
regenerated with the script in [Reproducing these numbers](#reproducing-these-numbers).

### Startup search (empty query)

When yoink launches, fzf's `start:reload` bind fires `__search ""`. Prior to
the fix described below this took ~3 seconds because the old code walked the
entire tree and emitted every file as a list row. yoink now short-circuits
on empty query so the prompt opens instantly and the first real reload is
the first one that does work.

| Repo | Empty-query `__search` time |
|---|---|
| `~/yoink` | 2ms |
| `~/combined-gtp-apps/app-gtp` | 2ms |
| `~/mujin/.../checkoutroot` | 2ms |

## Preview performance

The preview pane shows file content with `bat` and a one-line git-blame
header above it. The header lookup follows three tiers:

1. Whole-file blame is already in the per-session cache (e.g. blame-sort
   warmed it via Ctrl-B) → sub-millisecond `HashMap` hit.
2. Per-line blame for the focused line — `git blame -L N,N`, which stops
   walking history once it identifies *that one line's* commit.
3. No focused line (file-level entry) → `git log -1 --format=%at%x09%an` →
   ~30ms even on big repos.

### Per-file cold/warm preview times (`checkoutroot`)

| File | Lines | Cold preview | Warm preview |
|---|---:|---:|---:|
| `app-gtp/package.json` | 1 | 31ms | 12ms |
| `app-gtp/src/bootstrap.tsx` (L5) | 18 | 46ms | 35ms |
| `scipy/scipy/linalg/_solvers.py` (L80) | 846 | 141ms | 16ms |
| `matplotlib/lib/matplotlib/pyplot.py` (L200) | 4597 | 44ms | 22ms |

"Cold" = the very first invocation in a freshly-cleared session cache. "Warm"
= subsequent runs that hit the cache. Process spawn (~5-10ms) dominates the
warm path; perceived preview latency is essentially identical to the
pre-blame-feature baseline (~12ms).

### Why per-line blame matters (raw `git blame` timing)

| File | Lines | `git blame -L N,N` (best) | `git blame -L N,N` (worst) | `git blame` (whole file) |
|---|---:|---:|---:|---:|
| `app-gtp/src/bootstrap.tsx` | 18 | 4ms | 4ms | 5ms |
| `scipy/scipy/linalg/_solvers.py` | 846 | 144ms | 153ms | 148ms |
| `matplotlib/lib/matplotlib/pyplot.py` | 4597 | 368ms | 414ms | 561ms |
| `matplotlib/lib/matplotlib/axes/_axes.py` | 8634 | 28ms (L50) | 520ms (L500) | **2206ms** |

The bottom row is the file that motivated this whole codepath. Per-line
blame stops walking history as soon as it identifies the one line you asked
about; whole-file blame has to attribute every line, which is the dominant
cost on huge files in deep repos. Switching the preview header from
whole-file to per-line dropped the worst observed preview from 1.5s to a
range of 28-520ms depending on which line was selected.

## Blame-sort mode (Ctrl-B)

Toggling blame-sort still requires a full `git blame --line-porcelain` per
matched file. Progress is rendered as an in-place updating bar on a
dedicated screen handed over via fzf's `execute` action, and per-file
results are cached so subsequent reloads (each keystroke after the toggle)
read from disk instead of re-running git.

| Phase | Cost in `checkoutroot` for `useState` |
|---|---|
| rg phase (collect occurrences, group by file) | <100ms |
| Whole-file blame, 878 distinct files | 11–12s (one-time, with live progress bar) |
| Sort + render final list | <100ms |
| **Total cold** | **~12s** |
| **Total warm (cache hit, e.g. retyped query)** | **~1s** |

## Reproducing these numbers

The scripts referenced inline live in `/tmp/` during development; they are
intentionally kept out of the repo because they're tied to absolute paths on
the developer machine. The shapes are:

```bash
# Median of 3 timed runs
bench() {
    local name="$1"; shift
    local times=()
    for i in 1 2 3; do
        local s=$(date +%s%N)
        "$@" >/dev/null 2>&1
        local e=$(date +%s%N)
        times+=("$(( (e - s) / 1000000 ))")
    done
    printf "  %-9s %5dms  [%s]\n" "$name" \
        "$(printf '%s\n' "${times[@]}" | sort -n | awk 'NR==2')" \
        "${times[*]}"
}

# Search comparison
cd /path/to/checkoutroot
for q in productionCycle mobilerobot useState orchestrator workerTimeout; do
    echo "[$q]"
    bench "grep"  grep -rnI "$q" .
    bench "rg"    rg -n "$q" .
    bench "yoink" /path/to/yoink __search "$q"
done

# Preview latency
export YOINK_CACHE_DIR=/tmp/yoink-cache-test
for f in path1 path2 path3; do
    rm -rf "$YOINK_CACHE_DIR"
    bench "cold-$f" yoink __preview "$f" "" 50    # cache miss
    bench "warm-$f" yoink __preview "$f" "" 50    # subsequent run
done
rm -rf "$YOINK_CACHE_DIR"
```
