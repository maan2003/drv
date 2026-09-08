#!/usr/bin/env bash
set -euo pipefail
if [[ $# != 3 ]]; then
  echo 'usage: apply-exact-patch root patch-file log' >&2
  exit 64
fi
root=$1 patch_file=$2 log=$3
patch --batch --forward --fuzz=0 --no-backup-if-mismatch --verbose -d "$root" -p1 < "$patch_file" 2>&1 | tee -a "$log"
if grep -Ei '(fuzz|offset|FAILED|Reversed|previously applied|Skipping patch)' "$log"; then
  echo 'patch did not apply at its exact expected location' >&2
  exit 2
fi
if find "$root" -name '*.rej' -print | grep -q .; then
  find "$root" -name '*.rej' -print >&2
  exit 2
fi

