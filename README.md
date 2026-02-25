# yoink

`yoink` is an interactive terminal search tool powered by `ripgrep`, `fzf`, and `bat`.

It searches both:
- file/folder names (regex)
- text inside files (regex)

Results are shown on the left, with a live preview on the right.

## Install (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/AMitchell-GitHub/yoink/main/scripts/install-from-release.sh | bash -s -- AMitchell-GitHub/yoink
```

This installs:
- `yoink` to `~/.local/bin/yoink`
- default config to `~/.yoinkignore` (if it does not already exist)

If `yoink` is not found after install, add this to your shell config:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

## Requirements

- `rg` (ripgrep)
- `fzf`
- `bat`

Optional editor commands for keybinds:
- `vim`
- `code` (VS Code)
- `subl` (Sublime Text)

## Usage

```bash
yoink
yoink ejectReasons
```

## Keybinds

- `Enter`: print the containing directory of selected result
- `Ctrl-V`: open in `vim`
- `Ctrl-O`: open in `code`
- `Ctrl-S`: open in `subl`

## Optional shell helper so `yoink` can `cd`

`yoink` prints a path; a process cannot directly change your current shell directory.
If you want `yoink` itself to change directory, add this shell function to your `~/.zshrc` or `~/.bashrc`:

```bash
yoink() {
  local target
  target="$(command yoink "$@")" || return
  [[ -n "$target" ]] && cd "$target"
}
```

## Config (`~/.yoinkignore`)

`yoink` uses one system-wide config file at `~/.yoinkignore`.

Default:

```text
include_hidden=false
include_mounts=false
include_symlinks=false

.git/**
node_modukes/**
```

Behavior:
- `include_hidden`: include dotfiles and dot-directories
- `include_mounts`: search across mounted filesystems
- `include_symlinks`: follow symlinks
- Any other non-comment line is treated as an ignore glob
