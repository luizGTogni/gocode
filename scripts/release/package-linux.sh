#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <output-directory>" >&2
  exit 64
fi

output_dir=$1
workspace_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$workspace_dir"
build_target_dir=${CARGO_TARGET_DIR:-target}

version=$(cargo pkgid -p gocode | sed -E 's/.*[#@]//')
archive_name="gocode-${version}-linux-x86_64.tar.gz"
staging_dir=$(mktemp -d)
trap 'rm -rf "$staging_dir"' EXIT

cargo build --release --locked -p gocode
mkdir -p "$staging_dir/gocode-${version}-linux-x86_64" "$output_dir"
install -m 0755 "$build_target_dir/release/gocode" "$staging_dir/gocode-${version}-linux-x86_64/gocode"
cp LICENSE "$staging_dir/gocode-${version}-linux-x86_64/LICENSE"
cp docs/INSTALL.md "$staging_dir/gocode-${version}-linux-x86_64/INSTALL.md"
install -m 0755 scripts/install-linux.sh "$staging_dir/gocode-${version}-linux-x86_64/install-linux.sh"

tar -C "$staging_dir" -czf "$output_dir/$archive_name" "gocode-${version}-linux-x86_64"
