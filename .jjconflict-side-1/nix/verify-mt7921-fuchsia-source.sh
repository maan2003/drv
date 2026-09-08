#!/usr/bin/env bash
set -euo pipefail
if [[ $# != 6 ]]; then
  echo 'usage: verify root expected-base expected-patches expected-patch-set expected-closure origin-kind' >&2
  exit 64
fi
root=$1 expected_base=$2 expected_patches=$3 expected_patch_set=$4 expected_closure=$5 origin_kind=$6
[[ $origin_kind == derivation-owned ]] || { echo 'Fuchsia source origin is not derivation-owned' >&2; exit 2; }
[[ -d $root && ! -L $root ]] || { echo 'Fuchsia source root is absent or indirect' >&2; exit 2; }
[[ $(cat "$root/COMMIT") == "$expected_base" ]] || { echo 'Fuchsia base revision drifted' >&2; exit 2; }
[[ $(cat "$root/.drv-source-closure") == "$expected_closure" ]] || { echo 'Fuchsia source closure drifted' >&2; exit 2; }
cmp "$expected_patches" "$root/.drv-host-patches" || { echo 'ordered Fuchsia patch manifest drifted' >&2; exit 2; }
[[ $(sha256sum "$root/.drv-host-patches" | cut -d ' ' -f1) == "$expected_patch_set" ]] || { echo 'ordered Fuchsia patch-set hash drifted' >&2; exit 2; }
[[ $(cat "$root/.drv-host-patch-set") == "$expected_patch_set" ]] || { echo 'stale Fuchsia patch stamp' >&2; exit 2; }
generated_source_sha256=$(
  cd "$root"
  find . -type f \( -name '*.rs' -o -name Cargo.toml -o -name Cargo.lock \) -print0 | sort -z \
    | while IFS= read -r -d '' file; do printf '%s\0' "$file"; sha256sum "$file"; done \
    | sha256sum | cut -d ' ' -f1
)
materialized_tree_sha256=$(
  cd "$root"
  find . \( -type f -o -type l \) \
    ! -name .drv-materialized-source-tree-sha256 \
    ! -name .drv-generated-crate-source-sha256 \
    ! -name .drv-source-identity-sha256 -print0 | sort -z \
    | while IFS= read -r -d '' file; do
        printf '%s\0' "$file"
        if test -L "$file"; then readlink "$file"; else sha256sum "$file"; fi
      done | sha256sum | cut -d ' ' -f1
)
source_identity_sha256=$(
  printf '%s\n%s\n%s\n%s\n%s\n' "$expected_base" "$expected_closure" \
    "$expected_patch_set" "$materialized_tree_sha256" "$generated_source_sha256" \
    | sha256sum | cut -d ' ' -f1
)
[[ $(cat "$root/.drv-materialized-source-tree-sha256") == "$materialized_tree_sha256" ]] || { echo 'materialized source tree hash drifted' >&2; exit 2; }
[[ $(cat "$root/.drv-generated-crate-source-sha256") == "$generated_source_sha256" ]] || { echo 'generated crate source hash drifted' >&2; exit 2; }
[[ $(cat "$root/.drv-source-identity-sha256") == "$source_identity_sha256" ]] || { echo 'source identity drifted' >&2; exit 2; }
bound=$root/src/connectivity/wlan/lib/mlme/rust/src/client/bound.rs
grep -Fq 'listen_interval: 5' "$bound"
grep -Fq 'Id::RSNXE' "$bound"
! grep -Fq 'listen_interval: 0' "$bound" || { echo 'stale listen_interval=0 MLME' >&2; exit 2; }
