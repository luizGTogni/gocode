#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <gocode-<version>-linux-x86_64.tar.gz>" >&2
  exit 64
fi

archive=$1
if [[ ! -f $archive ]]; then
  echo "archive not found: $archive" >&2
  exit 66
fi

bin_dir="${XDG_BIN_HOME:-$HOME/.local/bin}"
staging_dir=$(mktemp -d)
trap 'rm -rf "$staging_dir"' EXIT
tar -xzf "$archive" -C "$staging_dir"
binary=$(find "$staging_dir" -type f -name gocode -print -quit)
if [[ -z $binary ]]; then
  echo "archive does not contain gocode" >&2
  exit 65
fi

install -d -m 0755 "$bin_dir"
install -m 0755 "$binary" "$bin_dir/gocode"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) echo "Installed gocode in $bin_dir. Add it to PATH, then open a new terminal." ;;
esac
