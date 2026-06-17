<img width="1962" height="1054" alt="image" src="https://github.com/user-attachments/assets/23ba01a8-63cd-4550-8e37-06e8b5296adb" />

# yoink ![GitHub all releases](https://img.shields.io/github/downloads/AMItchell-GitHub/yoink/total)

`yoink` is a native terminal search TUI powered by `ripgrep` (matching) and
`bat` (preview). It searches file/folder names and file contents together,
with a live preview, git blame sorting, and configurable open-actions.

The UI is its own ratatui app — no `fzf` dependency. Status, mode, and binds
are always visible; common settings flip with one keypress (no menu to open).

## Install (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/AMitchell-GitHub/yoink/refs/heads/master/scripts/install-from-release.sh | bash -s -- AMitchell-GitHub/yoink
```

This installs:
- `yoink` to `~/.local/bin/yoink`
- On first launch, yoink writes the fully-annotated default config to `~/.yoink-config`. Edit it freely; yoink reloads as soon as you save.

If `yoink` is not found after install, add this to your shell config:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

## Requirements

- `rg` (ripgrep)
- `bat`

Optional commands referenced by the default open-action keybindings:
- `vim` / `vi` / `nano` / `cat`
- `code` (VS Code)
- `subl` (Sublime Text)
- `xdg-open` (system file explorer / default app)
- `xclip` or `wl-copy` for clipboard binds

## Usage

```bash
yoink
yoink ejectReasons
```

Type your query, then **press Enter** to run the search. The default mode is
**case-insensitive glob** — a noob-friendly wrapper that translates to regex
under the hood, so you don't need to know regex to use yoink. Press **F2** to
switch to regex mode, toggle case sensitivity, or change sort order.

### Glob examples (default mode)
```
foo            → matches "foo" anywhere in name or content
*.rs           → matches the ".rs" suffix
src/*test*     → matches paths like src/foo_test.rs
foo?bar        → matches "fooXbar" for any single X
```
Regex metacharacters in a glob query (`.+(){}` etc.) are escaped automatically,
so a glob user can paste a filename with dots and it just works.

### Regex examples (switch via F2)
```
[Ss]earch                     → any char in []; matches: "search", "Search"
(?i)search                    → case-ignore inline (or just toggle Case in F2)
term1.*term2                  → multi-term; matches: "...term1...term2..."
(term1|term2).*(term1|term2)  → disordered multi-term
```

## Headless mode (scripting & LLMs)

Pass `--output/-o` and yoink skips the TUI entirely: it runs one search and
prints structured results to stdout. This is built for piping into other
programs — a shell script, `jq`, or an LLM — and as a way to export results to
paste somewhere else. (`yoink --help` prints this whole reference plus examples.)

```bash
yoink 'my.*search.*terms' -o json -m regex -s blame_young -C 10
yoink ejectReasons -o json               # glob (default), JSON output
yoink 'TODO' -o markdown > findings.md   # paste-ready export
```

Each content match carries the file path, line, column, the matched line, and
`-C/--context` lines of surrounding code on each side (default 10). With a
blame sort — or `--blame` — every result also gets its commit date, author, and
sha.

**`markdown` is the recommended format** — it's the most readable and compact,
for both humans and LLMs; reach for `json`/`jsonl` only when you need to parse
fields. For a broad query, cap the output with `--max-results 30` (or `100`) so
you don't get thousands of lines.

### Flags

| Flag | Values | Notes |
|------|--------|-------|
| `-o, --output` | `json` · `jsonl` · `markdown` · `text` | Triggers headless mode. |
| `-q, --query` | string | Query as a flag; use it when the query starts with `-`. |
| `-m, --mode` | `glob` · `regex` | Overrides the config search mode for this run. |
| `-s, --sort` | `depth` · `alphabetical` · `blame_young` · `blame_old` | Overrides the config sort. |
| `--case` | `sensitive` · `insensitive` | Overrides config case sensitivity. |
| `-C, --context` | N (default 10) | Lines of code each side of a content match. |
| `--max-results` | N | Caps results after sorting; envelope reports `truncated`. |
| `--blame` | — | Force blame info on for non-blame sorts too. |
| `--content-only` | — | Skip files/dirs that matched by name only. |

`-o`/`-s`/`-m`/`--case` only override the active session — your `~/.yoink-config`
is never modified.

### Quoting the query

There's no special delimiter — let your **shell** carry the query through, just
like `rg` or `grep`. Wrap it in **single quotes** so spaces, `"`, `$`, and regex
metacharacters arrive intact:

```bash
yoink 'fn main(' -o json -m regex          # spaces & parens
yoink 'name = "yoink"' -o text -m regex     # literal double-quotes
yoink -q '-C' -m regex -o json              # query starting with '-'
```

For a query that begins with `-`, use `-q/--query` (shown above) or put it after
a `--` separator: `yoink -o json -- '-C'`.

### Output formats

- **`json`** — one object: query metadata (`mode`, `sort`, `case`, `root`,
  `count`, `total_matches`, `truncated`) plus a `results` array. Each result:
  `kind` (`content` or `path`), `path`, `is_dir`, `line`, `column`, `match`
  (the hit line), `context_start_line`, `context` (the matched line plus the
  lines before and after, in order), and `blame`. Pretty-printed; fields that
  don't apply (e.g. `line`/`match`/`context` on a `path` hit) are omitted.
- **`jsonl`** — one result object per line, no envelope. Stream-friendly and
  easy to `grep`/`jq -c`.
- **`markdown`** — a heading per match with a fenced, line-numbered excerpt (the
  match line marked with `>`). Paste-ready.
- **`text`** — grep-style `path:line: match` with `path-line-` context lines.

## Keybinds

Always available, no menu required:

- **Enter** — run the search for the current query (does **not** open / cd)
- **↑ / ↓ / PgUp / PgDn / Home / End** — move the selection in the results pane
- **F3** — open the search-mode picker (glob / regex), cursor on the current value
- **F4** — open the case-sensitivity picker (insensitive / sensitive)
- **F5** — open the sort picker (depth / alphabetical / youngest blame / oldest blame)

  These three pickers are **session-only** — the choice applies until you quit and is never written to the config. To change the default for new sessions, use **F2 → edit the values → Save**.
- **F1** — show the full bindings list overlay (`?` is left free so it can be typed into the query — it's a glob wildcard and part of regex flags like `(?i)`)
- **F2** — settings overlay: edit the per-session defaults (search mode / sensitivity / sorting) with ←/→ and **Save** them, edit the config file, or view the shipped reference. Save writes the defaults for *new* sessions and leaves the current one alone.
- **Esc** — close any open overlay; with no overlay open, quit yoink (like Ctrl-C)
- **Ctrl-C** — quit

Everything else is opt-in via `bind.<key> = <action>` in `~/.yoink-config` — a key does nothing unless you bind it (there are no default `Ctrl-*` chords). Two kinds of actions: ones that act on the highlighted result, and ones that edit the query box. The shipped config wires up these:

- `Ctrl-D` — `cd` (into folder if highlighted, else to its parent; exits yoink)
- `Ctrl-U` — `clear_query` (clear the whole query)
- `Ctrl-O` — open in `code` (VS Code)
- `Ctrl-S` — open in `subl` (Sublime Text)
- `Ctrl-X` — open in system file explorer (`xdg-open`)
- `Ctrl-V` — open in `vim`
- `Ctrl-N` — open in `nano`
- `Ctrl-Y` — copy file path to clipboard
- `Ctrl-F` — copy file name to clipboard

Result-action vocabulary: `cd`, `vim`, `vi`, `nano`, `cat`, `vscode`, `sublime`,
`explorer`, `copy_path`, `copy_name`. Terminal editors (vim/vi/nano/cat) return
to the results list after you quit them; GUI editors / explorer / copies keep
the session open; `cd` exits and changes the shell's directory.

Query-editing vocabulary: `clear_query`, `delete_word`, `line_start`, `line_end`.
Bind these to whatever keys you like (e.g. `bind.ctrl-w = delete_word`); they're
unbound unless you add a line. If a key is claimed by more than one bind (or by
a built-in), the built-in — or else the first matching bind line — wins and the
rest do nothing; yoink lists every such collision in a startup warning.

Results list UX:
- Single mono-list: file/folder rows and text-match rows together
- Color/icon markers help quickly distinguish path hits, text hits, and mixed hits
- Main rows stay clean (icon + path), while occurrence lines appear underneath
- Occurrence count is shown once on the first occurrence line for each file
- Inline occurrence rows include line number + snippet and preview jumps directly to that line

## Optional shell helper so `yoink` can `cd`

`yoink` prints a path; a process cannot directly change your current shell directory.
If you want `yoink` itself to change directory, add this shell function to your `~/.zshrc` or `~/.bashrc`:

```bash
yoink() {
  # -o/--output, --help/--version, or piped stdout: run yoink directly so its
  # output reaches you untouched. The cd-capture below is for the TUI only.
  if [[ ! -t 1 ]]; then command yoink "$@"; return; fi
  local arg
  for arg in "$@"; do
    case "$arg" in -o*|--output*|-h|--help|-V|--version) command yoink "$@"; return ;; esac
  done
  local target
  target="$(command yoink "$@")" || return
  [[ -n "$target" ]] && cd "$target"
}
```

> **Important:** update any older wrapper you already have. Earlier versions
> capture `command yoink`'s stdout unconditionally, so `--output`, `--help`, and
> `--version` get fed to `cd` (producing `cd: no such file or directory: …`) and
> pipes like `yoink foo -o json | clipboard` break. The installer offers to
> update it for you.

## Config (`~/.yoink-config`)

`yoink` uses one system-wide config file at `~/.yoink-config`. On first
launch, yoink materializes a fully-annotated default file there so the
schema is self-documenting — every key has comments explaining its values
right next to it. Read the file once and you'll know everything.

Reopen the file from inside yoink with **F2 → "Edit config file"**, or
browse the shipped reference (read-only) with **F2 → "Show default config
(reference)"** if you've made your file messy and want to see the original.

**F2 → "Add headless guide to ~/.claude/CLAUDE.md"** drops a short crib sheet on
yoink's headless (`-o/--output`) mode into your global Claude Code instructions,
so an agent working in your repos knows how to drive yoink. It's written between
markers and refreshed in place on repeat use (no duplicates).

Sections in order:

- **defaults** — `search_mode` (glob | regex), `case_sensitive` (true | false), `sort` (depth | alphabetical | blame_young | blame_old), `update_check` (true | false). These are the *new-session* defaults; **F2 → Save** writes them. The inline **F3 / F4 / F5** pickers override them for the current session only and never touch the file.
- **walking** — `include_hidden`, `include_mounts`, `include_symlinks` (all `false` by default).
- **ignored paths** — glob patterns, one per line. Default: `.git/**`, `node_modules/**`.
- **keybinds** — `bind.<key> = <action>`. **Config-only**: a key has no behavior unless it's listed here, and that includes every `Ctrl-*` chord (no built-in defaults). Result actions: `cd`, `vim`, `vi`, `nano`, `cat`, `vscode`, `sublime`, `explorer`, `copy_path`, `copy_name`. Query-editing actions: `clear_query`, `delete_word`, `line_start`, `line_end`. If a key collides across binds (or with a built-in), the built-in — or else the first matching bind line — wins and the rest do nothing; yoink lists **every** such conflict in a startup warning.

Built-in keys (↑/↓/PgUp/PgDn/Esc/F1/F2/F3/F4/F5/Ctrl-C) are always available and can't be rebound. `Enter` is reserved — it always runs the search. `?` is deliberately *not* a built-in key so it can be typed into the query (glob wildcard / regex `(?i)` flags).

The easiest way to change the persisted defaults is **F2 → Settings** inside the tool; the inline **F3 / F4 / F5** pickers change only the current session.

## Staying up to date

On an interactive launch, yoink quietly checks GitHub for a newer release — at
most **once per day** (the result is cached, so it never slows a launch more
than that) and only when stdout is a real terminal (never in headless `-o`
runs, pipes, or scripts). If a newer version exists, yoink shows what changed
and offers to update itself:

- Accept and yoink runs the same installer from [Install](#install-recommended)
  for you — about **10 seconds**, minimal input — then **restarts into the new
  version** automatically.
- Decline and it won't ask again for that version.

Turn it off with `update_check = false` in `~/.yoink-config`. The check is
entirely best-effort: no network, no `curl`, or a failed request just falls
through to a normal launch.
