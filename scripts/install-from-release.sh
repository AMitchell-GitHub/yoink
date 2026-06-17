#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# Colors & helpers
# ---------------------------------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m'

info()  { printf "${BLUE}::${NC} %s\n" "$*"; }
ok()    { printf "${GREEN}::${NC} %s\n" "$*"; }
warn()  { printf "${YELLOW}::${NC} %s\n" "$*"; }
err()   { printf "${RED}::${NC} %s\n" "$*" >&2; }

# When piped via  curl … | bash  stdin is the pipe, so interactive reads
# must come from /dev/tty.
ask_yes_no() {
  local prompt="$1" answer
  while true; do
    printf "${BOLD}%s [y/n]:${NC} " "$prompt"
    read -r answer < /dev/tty
    case "$answer" in
      [Yy]|[Yy][Ee][Ss]) return 0 ;;
      [Nn]|[Nn][Oo])     return 1 ;;
      *) echo "  Please answer y or n." ;;
    esac
  done
}

# ---------------------------------------------------------------------------
# Args
# ---------------------------------------------------------------------------
if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <owner/repo> [version]"
  echo "Example: $0 AMitchell-GitHub/yoink"
  echo "Example: $0 AMitchell-GitHub/yoink v2.1.0"
  exit 1
fi

repo="$1"
version="${2:-latest}"

# ---------------------------------------------------------------------------
# 1. Download & install the binary
# ---------------------------------------------------------------------------
os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"

if [[ "$version" == "latest" ]]; then
  release_url="https://api.github.com/repos/${repo}/releases/latest"
  tag="$(curl -fsSL "$release_url" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
  if [[ -z "$tag" ]]; then
    err "Failed to resolve latest release tag for ${repo}"
    exit 1
  fi
else
  tag="$version"
fi

asset_name="yoink-${tag}-${os}-${arch}.tar.gz"
download_url="https://github.com/${repo}/releases/download/${tag}/${asset_name}"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

info "Downloading yoink ${tag}..."
curl -fL "$download_url" -o "$tmp_dir/yoink.tar.gz"

mkdir -p "$tmp_dir/unpack"
tar -xzf "$tmp_dir/yoink.tar.gz" -C "$tmp_dir/unpack"

install_dir="${HOME}/.local/bin"
mkdir -p "$install_dir"

binary_path="$(find "$tmp_dir/unpack" -type f -name yoink | head -n1)"
if [[ -z "$binary_path" ]]; then
  err "yoink binary not found in archive"
  exit 1
fi

install -m 0755 "$binary_path" "$install_dir/yoink"
ok "Installed yoink to ${install_dir}/yoink"

# ---------------------------------------------------------------------------
# 2. Default config
# ---------------------------------------------------------------------------
config_path="${HOME}/.yoink-config"
if [[ -f "$config_path" ]]; then
  info "Keeping existing config at ${config_path}"
else
  info "yoink will write the annotated default config to ${config_path} on first launch."
fi

# ---------------------------------------------------------------------------
# Detect the user's shell config file (used by the PATH and helper sections)
# ---------------------------------------------------------------------------
rc_file=""
shell_name=""

if [[ -n "${SHELL:-}" ]]; then
  case "$SHELL" in
    */zsh)  shell_name="zsh";  rc_file="${HOME}/.zshrc"  ;;
    */bash) shell_name="bash"; rc_file="${HOME}/.bashrc" ;;
  esac
fi

# Fallback: check which files exist
if [[ -z "$rc_file" ]]; then
  if [[ -f "${HOME}/.zshrc" ]]; then
    shell_name="zsh";  rc_file="${HOME}/.zshrc"
  elif [[ -f "${HOME}/.bashrc" ]]; then
    shell_name="bash"; rc_file="${HOME}/.bashrc"
  fi
fi

# ---------------------------------------------------------------------------
# 3. PATH check
# ---------------------------------------------------------------------------
if ! echo "$PATH" | tr ':' '\n' | grep -qx "$install_dir"; then
  echo
  warn "${install_dir} is not in your PATH."
  path_line='export PATH="$HOME/.local/bin:$PATH"'
  if [[ -n "$rc_file" ]] && grep -qF "$path_line" "$rc_file" 2>/dev/null; then
    info "${rc_file} already adds it to your PATH."
    echo "  Restart your shell (or run 'source ${rc_file}') to pick it up."
  elif [[ -n "$rc_file" ]]; then
    if ask_yes_no "  Add it to ${rc_file}?"; then
      printf '\n# Added by yoink installer: put ~/.local/bin on PATH\n%s\n' "$path_line" >> "$rc_file"
      ok "Added PATH update to ${rc_file}"
      echo
      warn "To use it in this terminal, run:"
      printf "    ${BOLD}source ${rc_file}${NC}\n"
    else
      echo "  You can add this line to your shell config manually:"
      printf "    ${BOLD}%s${NC}\n" "$path_line"
    fi
  else
    echo "  You may need to add this line to your shell config:"
    printf "    ${BOLD}%s${NC}\n" "$path_line"
  fi
fi

# ---------------------------------------------------------------------------
# 4. Dependency checks  (rg, bat)
# ---------------------------------------------------------------------------
echo
info "Checking dependencies..."
echo

# Detect package manager
pkg_manager=""
pkg_install_cmd=""
if command -v apt >/dev/null 2>&1; then
  pkg_manager="apt"
  pkg_install_cmd="sudo apt install -y"
elif command -v brew >/dev/null 2>&1; then
  pkg_manager="brew"
  pkg_install_cmd="brew install"
elif command -v dnf >/dev/null 2>&1; then
  pkg_manager="dnf"
  pkg_install_cmd="sudo dnf install -y"
elif command -v pacman >/dev/null 2>&1; then
  pkg_manager="pacman"
  pkg_install_cmd="sudo pacman -S --noconfirm"
elif command -v zypper >/dev/null 2>&1; then
  pkg_manager="zypper"
  pkg_install_cmd="sudo zypper install -y"
fi

# Map tool name -> package name for each package manager
pkg_name_for() {
  local tool="$1"
  case "$tool" in
    rg)
      case "$pkg_manager" in
        pacman) echo "ripgrep" ;;
        *)      echo "ripgrep" ;;
      esac ;;
    bat) echo "bat" ;;
  esac
}

missing_tools=()
missing_pkgs=()
batcat_handled=false

for tool in rg bat; do
  if command -v "$tool" >/dev/null 2>&1; then
    ok "${tool} found"
  else
    # On Debian/Ubuntu, bat is sometimes installed as 'batcat'
    if [[ "$tool" == "bat" ]] && command -v batcat >/dev/null 2>&1; then
      warn "'bat' is installed as 'batcat' on your system."
      echo "  yoink expects the command to be called 'bat'."
      echo "  A small symlink fixes this."
      echo
      if ask_yes_no "  Create symlink ~/.local/bin/bat -> $(command -v batcat)?"; then
        mkdir -p "${HOME}/.local/bin"
        ln -sf "$(command -v batcat)" "${HOME}/.local/bin/bat"
        ok "Created symlink: ~/.local/bin/bat -> $(command -v batcat)"
        batcat_handled=true
      else
        warn "Skipped. You can create the symlink manually later:"
        echo "    mkdir -p ~/.local/bin"
        echo "    ln -s $(command -v batcat) ~/.local/bin/bat"
      fi
      echo
    else
      missing_tools+=("$tool")
      missing_pkgs+=("$(pkg_name_for "$tool")")
      err "${tool} is NOT installed  (required by yoink)"
      case "$tool" in
        rg)  echo "    ripgrep - a fast regex search tool" ;;
        bat) echo "    bat    - a syntax-highlighted file viewer" ;;
      esac
    fi
  fi
done

if [[ ${#missing_tools[@]} -gt 0 ]]; then
  echo
  if [[ -n "$pkg_manager" ]]; then
    info "You can install the missing dependencies with:"
    printf "    ${BOLD}${pkg_install_cmd} ${missing_pkgs[*]}${NC}\n"
    echo
    if ask_yes_no "  Install them now?"; then
      echo
      info "Running: ${pkg_install_cmd} ${missing_pkgs[*]}"
      # shellcheck disable=SC2086
      $pkg_install_cmd ${missing_pkgs[*]}
      echo

      # After installing via apt, bat may have landed as 'batcat'
      if [[ " ${missing_tools[*]} " == *" bat "* ]] && [[ "$pkg_manager" == "apt" ]]; then
        if command -v batcat >/dev/null 2>&1 && ! command -v bat >/dev/null 2>&1; then
          echo
          warn "On your system, bat was installed as 'batcat'."
          echo "  yoink expects the command to be called 'bat'."
          echo
          if ask_yes_no "  Create symlink ~/.local/bin/bat -> /usr/bin/batcat?"; then
            mkdir -p "${HOME}/.local/bin"
            ln -sf /usr/bin/batcat "${HOME}/.local/bin/bat"
            ok "Created symlink: ~/.local/bin/bat -> /usr/bin/batcat"
          else
            warn "Skipped. You can create the symlink manually later:"
            echo "    mkdir -p ~/.local/bin"
            echo "    ln -s /usr/bin/batcat ~/.local/bin/bat"
          fi
          echo
        fi
      fi

      ok "Dependencies installed."
    else
      warn "Skipped automatic install. Please install them before running yoink."
    fi
  else
    warn "Could not detect a package manager on your system."
    echo "  Please install the missing tools manually:"
    echo
    for tool in "${missing_tools[@]}"; do
      case "$tool" in
        rg)  echo "    ripgrep  https://github.com/BurntSushi/ripgrep#installation" ;;
        bat) echo "    bat      https://github.com/sharkdp/bat#installation" ;;
      esac
    done
  fi
  echo
fi

# ---------------------------------------------------------------------------
# 5. Shell helper function  (lets yoink cd into directories)
# ---------------------------------------------------------------------------
# Version being installed, stamped into the managed block so a later install
# can see which version wrote it. Taken from the release tag resolved above.
YOINK_VERSION="${tag#v}"
[[ -n "$YOINK_VERSION" ]] || YOINK_VERSION="unknown"

# The canonical shell function yoink manages in your rc file, delimited by the
# markers below and stamped with the installing version. Markers + version mean
# every future install can find and rewrite it *in place* — whether the user
# has the current version, an older marked version, or the legacy unmarked
# function from before these markers existed. Any version bump (or body change)
# makes the canonical block differ from what's there, so the diff below detects
# it and offers the update.
YOINK_FN_BEGIN='# >>> yoink shell function (managed by yoink installer - do not edit) >>>'

emit_yoink_block() {
  sed "s/__YOINK_VERSION__/${YOINK_VERSION}/g" <<'FUNC'
# >>> yoink shell function (managed by yoink installer - do not edit) >>>
# Created by yoink v__YOINK_VERSION__.
# You can move this whole section anywhere in your profile. Editing inside it is
# not recommended -- your changes will likely be overwritten on the next yoink
# update.
# Behavior: in the interactive TUI this cd's into the selected result; with
# -o/--output or when the output is piped, yoink runs directly so its results
# pass straight through.
yoink() {
  if [[ ! -t 1 ]]; then command yoink "$@"; return; fi
  local arg
  for arg in "$@"; do
    case "$arg" in -o*|--output*) command yoink "$@"; return ;; esac
  done
  local target
  target="$(command yoink "$@")" || return
  [[ -n "$target" ]] && cd "$target"
}
# <<< yoink shell function <<<
FUNC
}

# Emit $1's contents with the managed block inserted or updated *in place*:
#   * marked block present   -> replace the lines between the markers
#   * legacy yoink() present -> replace the function (+ its leading "# yoink"
#                               comments) where it sits, wrapping it in markers
#   * neither                -> append the block after a blank-line separator
build_proposed_rcfile() {
  local rc="$1" canon="$2"
  if [[ -f "$rc" ]] && grep -qF -- "$YOINK_FN_BEGIN" "$rc"; then
    awk -v canon="$canon" '
      BEGIN { while ((getline l < canon) > 0) c[++n] = l }
      index($0, "# >>> yoink shell function") == 1 { for (i=1;i<=n;i++) print c[i]; inb=1; next }
      inb { if (index($0, "# <<< yoink shell function") == 1) inb=0; next }
      { print }
    ' "$rc"
  elif [[ -f "$rc" ]] && grep -q '^yoink()[[:space:]]*{' "$rc"; then
    awk -v canon="$canon" '
      BEGIN { while ((getline l < canon) > 0) c[++n] = l }
      /^[[:space:]]*#[[:space:]]*yoink/ { pending = pending $0 ORS; next }
      skip == 1 { if ($0 ~ /^}/) { skip = 0; for (i=1;i<=n;i++) print c[i] } next }
      /^yoink\(\)[[:space:]]*\{/ { pending = ""; skip = 1; next }
      { if (pending != "") { printf "%s", pending; pending = "" } print }
      END { if (pending != "") printf "%s", pending }
    ' "$rc"
  else
    if [[ -f "$rc" ]]; then
      cat "$rc"
      if [[ -s "$rc" && -n "$(tail -c1 "$rc")" ]]; then printf '\n'; fi
      printf '\n'
    fi
    cat "$canon"
  fi
}

echo
if [[ -z "$rc_file" ]]; then
  warn "Could not detect your shell config file."
  echo "  Add this to your ~/.bashrc or ~/.zshrc so 'yoink' can cd into results:"
  echo
  emit_yoink_block | sed 's/^/    /'
  echo
else
  yoink_canon="$(mktemp)"
  yoink_proposed="$(mktemp)"
  emit_yoink_block > "$yoink_canon"
  build_proposed_rcfile "$rc_file" "$yoink_canon" > "$yoink_proposed"
  yoink_base="$rc_file"
  [[ -f "$yoink_base" ]] || yoink_base=/dev/null

  if diff -q "$yoink_base" "$yoink_proposed" >/dev/null 2>&1; then
    ok "yoink() shell function is already up to date in ${rc_file}"
  else
    existing_ver="$(grep -m1 -oE 'Created by yoink v[0-9]+(\.[0-9]+)*' "$rc_file" 2>/dev/null | grep -oE '[0-9]+(\.[0-9]+)*' | head -n1 || true)"
    if [[ -n "$existing_ver" && "$existing_ver" != "$YOINK_VERSION" ]]; then
      info "Updating the yoink shell function: v${existing_ver} -> v${YOINK_VERSION}."
    elif [[ -n "$existing_ver" ]]; then
      info "Refreshing the yoink shell function (v${existing_ver}; it looks modified)."
    else
      info "Installing the yoink shell function (v${YOINK_VERSION})."
    fi
    info "It cd's into a result in the TUI; headless (-o/--output) and piped output pass through."
    echo
    echo "  Proposed changes to ${rc_file}:"
    echo
    # Indent and colorize the unified diff: green additions, red removals, blue
    # hunk headers, bold file headers. (awk's -v turns the \033 in the color
    # vars into real escapes.)
    diff -u "$yoink_base" "$yoink_proposed" 2>/dev/null | awk \
      -v g="$GREEN" -v r="$RED" -v b="$BLUE" -v bold="$BOLD" -v nc="$NC" '
      /^\+\+\+/ || /^---/ { printf "    %s%s%s\n", bold, $0, nc; next }
      /^@@/               { printf "    %s%s%s\n", b, $0, nc; next }
      /^\+/               { printf "    %s%s%s\n", g, $0, nc; next }
      /^-/                { printf "    %s%s%s\n", r, $0, nc; next }
      { printf "    %s\n", $0 }
    ' || true
    echo
    if ask_yes_no "  Apply these changes to ${rc_file}?"; then
      if [[ -f "$rc_file" ]]; then
        if cp "$rc_file" "${rc_file}.yoink.bak"; then
          info "Backed up previous config to ${rc_file}.yoink.bak"
        else
          warn "Could not back up ${rc_file} (continuing anyway)."
        fi
      fi
      cat "$yoink_proposed" > "$rc_file"
      ok "Updated the yoink() function in ${rc_file}"
      echo
      warn "To start using it in this terminal, run:"
      printf "    ${BOLD}source ${rc_file}${NC}\n"
      echo
      echo "  Or just open a new terminal window."
    else
      info "Skipped. No changes were made to ${rc_file}."
    fi
  fi
  rm -f "$yoink_canon" "$yoink_proposed"
fi

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------
echo
ok "Setup complete! Run 'yoink' to get started."
