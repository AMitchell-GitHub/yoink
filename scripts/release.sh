#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is required but was not found in PATH" >&2
  exit 1
fi

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$workspace_root"

version="$(grep -E '^version\s*=\s*"' Cargo.toml | head -n1 | sed -E 's/version\s*=\s*"([^"]+)"/\1/')"
if [[ -z "$version" ]]; then
  echo "error: failed to read version from Cargo.toml" >&2
  exit 1
fi

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
artifact_name="yoink-v${version}-${os}-${arch}"

echo "Building release binary..."
cargo build --release --locked

binary_path="target/release/yoink"
if [[ ! -x "$binary_path" ]]; then
  echo "error: expected binary not found at $binary_path" >&2
  exit 1
fi

dist_dir="dist"
package_dir="${dist_dir}/${artifact_name}"
mkdir -p "$package_dir"

cp "$binary_path" "$package_dir/yoink"
cp README.md "$package_dir/README.md"
cp LICENSE "$package_dir/LICENSE"

archive_path="${dist_dir}/${artifact_name}.tar.gz"
tar -C "$dist_dir" -czf "$archive_path" "$artifact_name"

checksum_path="${archive_path}.sha256"
sha256sum "$archive_path" > "$checksum_path"

echo
echo "Release artifacts created:"
echo "  $archive_path"
echo "  $checksum_path"
