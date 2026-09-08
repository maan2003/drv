#!@shell@
set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo "usage: $0 MANIFEST ROOTS OUTPUT-DIRECTORY NIX-STORE" >&2
  exit 64
fi

manifest=$1
roots_file=$2
output=$3
nix_store=$4
paths=$output/closure.expected.paths
actual_paths=$output/closure.actual.paths
verified=$output/closure.verified.tsv

@validator@ "$manifest" "$paths"
mapfile -t roots <"$roots_file"
"$nix_store" -qR "${roots[@]}" | @sort@ -u >"$actual_paths"
@cmp@ "$paths" "$actual_paths"
: >"$verified"
path=
expected=
while IFS=$'\t' read -r path expected; do
  "$nix_store" --verify-path "$path"
  if ! actual=$("$nix_store" -q --hash "$path"); then
    echo "registered hash query failed: $path" >&2
    exit 1
  fi
  if [ -z "$actual" ] || [ "$actual" != "$expected" ]; then
    echo "registered hash mismatch: $path" >&2
    exit 1
  fi
  printf '%s\t%s\n' "$path" "$actual" >>"$verified"
done <"$manifest"
if [ -n "$path" ] || [ -n "$expected" ]; then
  echo "closure manifest must end with a newline" >&2
  exit 1
fi
