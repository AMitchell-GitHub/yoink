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

ask_yes_no() {
  local prompt="$1" answer
  while true; do
    printf "${BOLD}%s [y/n]:${NC} " "$prompt"
    read -r answer
    case "$answer" in
      [Yy]|[Yy][Ee][Ss]) return 0 ;;
      [Nn]|[Nn][Oo])     return 1 ;;
      *) echo "  Please answer y or n." ;;
    esac
  done
}

# ---------------------------------------------------------------------------
# 1. Build from source
# ---------------------------------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
  err "cargo is required but was not found in PATH."
  echo "  Install Rust: https://rustup.rs"
  exit 1
fi

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$workspace_root"

info "Building yoink with cargo..."
cargo install --path . --locked --force
ok "yoink installed via cargo"

# ---------------------------------------------------------------------------
# 2. Default config
# ---------------------------------------------------------------------------
config_path="$HOME/.yoinkignore"
default_config="$workspace_root/.yoinkignore"

if [[ ! -f "$config_path" ]]; then
  cp "$default_config" "$config_path"
  ok "Installed default config at $config_path"
else
  info "Keeping existing config at $config_path"
fi

# ---------------------------------------------------------------------------
# 3. PATH check
# ---------------------------------------------------------------------------
cargo_bin="${HOME}/.cargo/bin"
if ! echo "$PATH" | tr ':' '\n' | grep -qx "$cargo_bin"; then
  echo
  warn "${cargo_bin} is not in your PATH."
  echo "  You may need to add this line to your shell config:"
  printf "    ${BOLD}export PATH=\"\$HOME/.cargo/bin:\$PATH\"${NC}\n"
fi

# ---------------------------------------------------------------------------
# 4. Dependency checks  (rg, fzf, bat)
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

pkg_name_for() {
  local tool="$1"
  case "$tool" in
    rg)  echo "ripgrep" ;;
    fzf) echo "fzf" ;;
    bat) echo "bat" ;;
  esac
}

missing_tools=()
missing_pkgs=()

for tool in rg fzf bat; do
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
        fzf) echo "    fzf    - a fuzzy finder for the terminal" ;;
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
        fzf) echo "    fzf      https://github.com/junegunn/fzf#installation" ;;
        bat) echo "    bat      https://github.com/sharkdp/bat#installation" ;;
      esac
    done
  fi
  echo
fi

# ---------------------------------------------------------------------------
# 5. Shell helper function  (lets yoink cd into directories)
# ---------------------------------------------------------------------------
echo
info "Optional: shell helper function"
echo
echo "  By default, running 'yoink' prints the path of the selected result."
echo "  If you'd like yoink to automatically cd into the result's directory,"
echo "  a small wrapper function is needed in your shell config."
echo
echo "  The function:"
echo
printf "    ${BOLD}yoink() {${NC}\n"
printf "    ${BOLD}  local target${NC}\n"
printf "    ${BOLD}  target=\"\$(command yoink \"\$@\")\" || return${NC}\n"
printf "    ${BOLD}  [[ -n \"\$target\" ]] && cd \"\$target\"${NC}\n"
printf "    ${BOLD}}${NC}\n"
echo
echo "  It runs the real yoink binary, captures the path it outputs,"
echo "  then cd's your shell into that directory. Without this, yoink"
echo "  can only print the path (a subprocess cannot change the parent"
echo "  shell's working directory)."
echo

# Detect the user's shell config file
rc_file=""
shell_name=""

if [[ -n "${SHELL:-}" ]]; then
  case "$SHELL" in
    */zsh)  shell_name="zsh";  rc_file="${HOME}/.zshrc"  ;;
    */bash) shell_name="bash"; rc_file="${HOME}/.bashrc" ;;
  esac
fi

if [[ -z "$rc_file" ]]; then
  if [[ -f "${HOME}/.zshrc" ]]; then
    shell_name="zsh";  rc_file="${HOME}/.zshrc"
  elif [[ -f "${HOME}/.bashrc" ]]; then
    shell_name="bash"; rc_file="${HOME}/.bashrc"
  fi
fi

if [[ -n "$rc_file" ]]; then
  if grep -q 'command yoink' "$rc_file" 2>/dev/null; then
    ok "yoink() shell function already exists in ${rc_file}"
  else
    if ask_yes_no "  Add the yoink() function to ${rc_file}?"; then
      cat >> "$rc_file" <<'FUNC'

# yoink: cd into the directory of the selected search result
yoink() {
  local target
  target="$(command yoink "$@")" || return
  [[ -n "$target" ]] && cd "$target"
}
FUNC
      ok "Added yoink() function to ${rc_file}"
      echo
      warn "To start using it in this terminal, run:"
      printf "    ${BOLD}source ${rc_file}${NC}\n"
      echo
      echo "  Or just open a new terminal window."
    else
      info "Skipped. You can always add it manually later."
    fi
  fi
else
  warn "Could not detect your shell config file."
  echo "  Add the yoink() function shown above to your ~/.bashrc or ~/.zshrc manually."
fi

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------
echo
ok "Setup complete! Run 'yoink' to get started."
