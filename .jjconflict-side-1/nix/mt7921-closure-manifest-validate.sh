#!@shell@
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 MANIFEST OUTPUT-PATHS" >&2
  exit 64
fi

manifest=$1
output_paths=$2
tmp=$(@mktemp@ -d)
trap 'rm -rf "$tmp"' EXIT
declare -A seen_manifest=()

: >"$tmp/manifest.paths"
line=
while IFS= read -r line; do
  if [[ "$line" != *$'\t'* || "$line" == *$'\t'*$'\t'* ]]; then
    echo "manifest row must contain exactly two tab-separated fields" >&2
    exit 1
  fi
  path=${line%%$'\t'*}
  hash=${line#*$'\t'}
  if [[ ! "$path" =~ ^/nix/store/[0-9abcdfghijklmnpqrsvwxyz]{32}-[^/[:space:]]+$ ]] ||
     [[ ! "$hash" =~ ^sha256:[0123456789abcdfghijklmnpqrsvwxyz]{52}$ ]]; then
    echo "malformed closure manifest row" >&2
    exit 1
  fi
  if [[ -n "${seen_manifest[$path]+present}" ]]; then
    echo "duplicate closure manifest path: $path" >&2
    exit 1
  fi
  seen_manifest[$path]=1
  printf '%s\n' "$path" >>"$tmp/manifest.paths"
done <"$manifest"
if [ -n "$line" ]; then
  echo "closure manifest must end with a newline" >&2
  exit 1
fi
@sort@ -o "$tmp/manifest.paths" "$tmp/manifest.paths"
mv "$tmp/manifest.paths" "$output_paths"
