#!/usr/bin/env bash
# Incremental out-of-nix build of the MT7921 full-firmware validation binary
# on no-plastic. Produces a directory with the same layout as the nix package
# (bin/ launcher, libexec/ driver, share/ identity) so regen-and-run.sh and
# launch-active-run.sh accept it in place of a store path.
#
#   fast-build.sh            -> prints the output dir
#   fast-build.sh --refresh  -> also regenerate the dev-shell env + reference tree
#
# Layout on np:
#   $REPO   /data/persist/src/drv          rsync target of the workspace
#   $WORK   /data/persist/src/drv-fast     composed tree: crates/ + reference/
#   target  /data/persist/src/drv-fast-target (incremental cargo cache)
set -euo pipefail
REPO=${REPO:-/data/persist/src/drv}
WORK=${WORK:-/data/persist/src/drv-fast}
OUT=${OUT:-/data/persist/drvlab/fast-out}
export CARGO_HOME=${CARGO_HOME:-/data/persist/src/drv-fast-cargo}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/data/persist/src/drv-fast-target}
export CARGO_INCREMENTAL=1
refresh=${1:-}
t0=$(date +%s)
step(){ printf "%s +%ss %s\n" "$(date -u +%T)" "$(( $(date +%s) - t0 ))" "$*"; }
mkdir -p "$WORK" "$OUT"

# 1. dev-shell environment (cached; regenerate on --refresh or flake change)
envfile=$WORK/.dev-env.sh
if [ "$refresh" = --refresh ] || [ ! -s "$envfile" ] || [ "$REPO/flake.nix" -nt "$envfile" ]; then
  step "regenerating dev shell env"
  (cd "$REPO" && nix print-dev-env .#mt7921 > "$envfile.tmp") && mv "$envfile.tmp" "$envfile"
fi
# shellcheck disable=SC1090
source "$envfile" >/dev/null 2>&1 || true
step "toolchain: $(cargo --version) / $(rustc --version)"

# 2. patched Fuchsia reference tree: rebuilt by nix only when the patch set
#    (or its nix description) changes; the nix copy of the repo costs ~15 s.
patchsig=$( { cat "$REPO/nix/mt7921-fuchsia-source.nix" "$REPO/nix/fuchsia-reference.json"; \
             cat "$REPO"/crates/netstack3-port-spike/upstream-cargo/patches/*.patch; } | sha256sum | cut -d' ' -f1)
if [ "$refresh" = --refresh ] || [ ! -d "$WORK/reference" ] || [ "$(cat "$WORK/.patchsig" 2>/dev/null)" != "$patchsig" ]; then
  step "rebuilding patched reference tree via nix"
  src=$(cd "$REPO" && nix build --offline --print-out-paths --no-link .#mt7921-fuchsia-source)
  rsync -a --delete --chmod=u+w "$src/reference/" "$WORK/reference/"
  cp "$src/share/mt7921-fuchsia-source/identity.json" "$WORK/.identity.json"
  echo "$patchsig" > "$WORK/.patchsig"
  step "reference tree ready from $src"
fi
ref=$(ls -d "$WORK"/reference/fuchsia-* | head -1)

# 3. project crates straight from the rsynced workspace (no nix copy)
rsync -a --delete --exclude target "$REPO/crates/" "$WORK/crates/"
cp "$REPO/Cargo.toml" "$REPO/Cargo.lock" "$WORK/"   # root workspace: crates inherit edition/version from it
step "crates synced"

# 4. identity environment, mirroring the nix preBuild
export MT7921_FUCHSIA_BASE_REVISION=$(cat "$ref/COMMIT")
export MT7921_FUCHSIA_ORDERED_PATCH_SET_SHA256=$(cat "$ref/.drv-host-patch-set")
export MT7921_FUCHSIA_ORDERED_PATCH_LIST=$(sed -n 's/.*"ordered_patch_list":"\([^"]*\)".*/\1/p' "$WORK/.identity.json")
export MT7921_MATERIALIZED_SOURCE_TREE_SHA256=$(cat "$ref/.drv-materialized-source-tree-sha256")
export MT7921_GENERATED_CRATE_SOURCE_SHA256=$(cat "$ref/.drv-generated-crate-source-sha256")
export MT7921_SOURCE_IDENTITY_SHA256=$(cat "$ref/.drv-source-identity-sha256")
export MT7921_PROJECT_CORE_SOURCE_SHA256=$(cd "$WORK" && find crates/mt7921-core -type f -print0 | sort -z \
  | while IFS= read -r -d $'\0' file; do printf '%s\0' "$file"; sha256sum "$file"; done | sha256sum | cut -d ' ' -f1)
export MT7921_COMPOSITE_ARTIFACT_SOURCE_SHA256=$(printf '%s\n%s\n%s\n' "$MT7921_SOURCE_IDENTITY_SHA256" \
  "$MT7921_PROJECT_CORE_SOURCE_SHA256" connac2-bss-wire-v1 | sha256sum | cut -d ' ' -f1)
export session_client_mac=8a:fd:2a:8b:70:5a

# 5. incremental cargo build (same flags as the nix package)
cd "$WORK/crates/mt7921-passive-scan"
step "cargo build"
offline=--offline; [ -d "$CARGO_HOME/registry" ] || offline=
cargo build --release $offline --no-default-features --features fuchsia-passive,full-firmware-production 2>&1 \
  | grep -vE "^\s*(Compiling|Finished|warning: unused|Fresh)" | grep -E "error|warning: .*never|Finished|panicked" || true
driver=$CARGO_TARGET_DIR/release/mt7921-passive-scan
[ -x "$driver" ] || { echo "build failed: no $driver"; exit 1; }
step "binary ready"

# 6. package like the nix output; reuse the last nix launcher with paths rewritten
last=$(readlink -f "$REPO/result" 2>/dev/null || true)
[ -n "$last" ] && [ -x "$last/bin/mt7921-full-firmware-validation" ] || { echo "need a prior nix result at $REPO/result for the launcher template"; exit 1; }
rm -rf "$OUT"; mkdir -p "$OUT/bin" "$OUT/libexec" "$OUT/share/mt7921-full-firmware-validation"
install -m0755 "$driver" "$OUT/libexec/mt7921-full-firmware-validation"
"$OUT/libexec/mt7921-full-firmware-validation" --artifact-identity > "$OUT/share/mt7921-full-firmware-validation/artifact-identity.json"
cp "$last"/share/mt7921-full-firmware-validation/mock-ph1.psk "$OUT/share/mt7921-full-firmware-validation/"
sed "s|$last|$OUT|g" "$last/bin/mt7921-full-firmware-validation" > "$OUT/bin/mt7921-full-firmware-validation"
chmod +x "$OUT/bin/mt7921-full-firmware-validation"
ln -s ../libexec/mt7921-full-firmware-validation "$OUT/bin/mt7921-full-firmware-validation-driver"
echo "$(sha256sum "$driver" | cut -c1-16) $(date -u +%FT%TZ)" > "$OUT/BUILD"
step "done: $OUT (driver sha256 $(cut -c1-16 "$OUT/BUILD"))"
echo "$OUT"
