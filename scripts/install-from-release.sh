#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <owner/repo> [version]"
  echo "Example: $0 aidan/yoink"
  echo "Example: $0 aidan/yoink v0.1.0"
  exit 1
fi

repo="$1"
version="${2:-latest}"

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"

if [[ "$version" == "latest" ]]; then
  release_url="https://api.github.com/repos/${repo}/releases/latest"
  tag="$(curl -fsSL "$release_url" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
  if [[ -z "$tag" ]]; then
    echo "error: failed to resolve latest release tag for ${repo}" >&2
    exit 1
  fi
else
  tag="$version"
fi

asset_name="yoink-${tag}-${os}-${arch}.tar.gz"
download_url="https://github.com/${repo}/releases/download/${tag}/${asset_name}"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

echo "Downloading ${download_url}"
curl -fL "$download_url" -o "$tmp_dir/yoink.tar.gz"

mkdir -p "$tmp_dir/unpack"
tar -xzf "$tmp_dir/yoink.tar.gz" -C "$tmp_dir/unpack"

install_dir="${HOME}/.local/bin"
mkdir -p "$install_dir"

binary_path="$(find "$tmp_dir/unpack" -type f -name yoink | head -n1)"
if [[ -z "$binary_path" ]]; then
  echo "error: yoink binary not found in archive" >&2
  exit 1
fi

install -m 0755 "$binary_path" "$install_dir/yoink"
echo "Installed yoink to ${install_dir}/yoink"

config_path="${HOME}/.yoinkignore"
if [[ ! -f "$config_path" ]]; then
  cat > "$config_path" <<'EOF'
include_hidden=false
include_mounts=false
include_symlinks=false
sort_mode=depth

.git/**
node_modukes/**
EOF
  echo "Installed default config at ${config_path}"
else
  echo "Keeping existing config at ${config_path}"
fi

if ! command -v yoink >/dev/null 2>&1; then
  echo
  echo "Add ${install_dir} to PATH if needed:"
  echo '  export PATH="$HOME/.local/bin:$PATH"'
fi

echo "Done. Run: yoink"
