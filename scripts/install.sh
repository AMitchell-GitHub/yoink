#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is required but was not found in PATH" >&2
  exit 1
fi

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$workspace_root"

echo "Installing yoink with cargo..."
cargo install --path . --locked --force

config_path="$HOME/.yoinkignore"
default_config="$workspace_root/.yoinkignore"

if [[ ! -f "$config_path" ]]; then
  cp "$default_config" "$config_path"
  echo "Installed default config at $config_path"
else
  echo "Keeping existing config at $config_path"
fi

echo
echo "yoink installed."
echo "If needed, add ~/.cargo/bin to your PATH:"
echo '  export PATH="$HOME/.cargo/bin:$PATH"'
