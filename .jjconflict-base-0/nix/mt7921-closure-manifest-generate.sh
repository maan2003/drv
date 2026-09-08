#!@shell@
set -euo pipefail

if [ "$#" -lt 4 ] || [ "$3" != -- ]; then
  echo "usage: $0 OUTPUT PATHS -- HASH-COMMAND [ARG ...]" >&2
  exit 64
fi

output=$1
paths=$2
shift 3
tmp="$output.tmp"
trap 'rm -f "$tmp"' EXIT
: >"$tmp"

while IFS= read -r path; do
  if [ -z "$path" ]; then
    echo "closure path must not be empty" >&2
    exit 1
  fi
  if ! hash=$("$@" "$path"); then
    echo "failed to hash closure path: $path" >&2
    exit 1
  fi
  case "$hash" in
    *$'\n'* | *$'\t'* | *' '*)
      echo "closure hash must be one nonempty field: $path" >&2
      exit 1
      ;;
    '')
      echo "closure hash must not be empty: $path" >&2
      exit 1
      ;;
  esac
  printf '%s\t%s\n' "$path" "$hash" >>"$tmp"
done <"$paths"

mv "$tmp" "$output"
trap - EXIT
