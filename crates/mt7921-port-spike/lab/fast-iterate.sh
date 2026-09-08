#!/usr/bin/env bash
# Local dev loop: rsync the workspace to np, incremental cargo build there
# (fast-build.sh), optionally launch an active run with the result.
#   fast-iterate.sh            sync + build, prints the output dir on np
#   fast-iterate.sh --run      ... then launch-active-run.sh <out>
#   fast-iterate.sh --test 'cargo test args'   sync, then run cargo in the
#                              root workspace of the composed tree on np
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
cfg=$HOME/.ssh/lab.conf
t0=$(date +%s)
# np is reachable over Tailscale only when its sole network, ajay, is up.
target=np
echo "target=$target"
rsync -az --delete --exclude target --exclude .jj --exclude .git --exclude 'result*' --exclude reference \
  -e "ssh -F $cfg -o ConnectTimeout=15" \
  "$root/crates" "$root/Cargo.toml" "$root/Cargo.lock" "$root/flake.nix" "$root/flake.lock" "$root/nix" "$root/AGENTS.md" \
  "$target":/data/persist/src/drv/
echo "synced +$(( $(date +%s) - t0 ))s"
if [ "${1:-}" = --test ]; then
  ssh -n -F "$cfg" "$target" "set -e; source /data/persist/src/drv-fast/.dev-env.sh >/dev/null 2>&1; \
    rsync -a --delete --exclude target /data/persist/src/drv/crates/ /data/persist/src/drv-fast/crates/; \
    cp /data/persist/src/drv/Cargo.toml /data/persist/src/drv/Cargo.lock /data/persist/src/drv-fast/; \
    cd /data/persist/src/drv-fast && CARGO_HOME=/data/persist/src/drv-fast-cargo CARGO_TARGET_DIR=/data/persist/src/drv-fast-target-root \
    cargo ${2:-test -p mt7921-core} 2>&1 | grep -vE '^\s*(Compiling|Downloading|Downloaded|Locking|Adding|warning: unused)' | tail -n ${LINES_TAIL:-40}"
  exit
fi
ssh -n -F "$cfg" "$target" 'cp /data/persist/src/drv/crates/mt7921-port-spike/lab/fast-build.sh /data/persist/drvlab/fast-build.sh; /data/persist/drvlab/fast-build.sh 2>&1 | tee /data/persist/drvlab/fast-build-last.log | grep -E "^[0-9:]+ \+|^error|^\s+-->" | tail -n 30'
out=$(ssh -n -F "$cfg" "$target" 'tail -n1 /data/persist/drvlab/fast-build-last.log')
echo "build done +$(( $(date +%s) - t0 ))s out=$out"
[ "${1:-}" = --run ] && exec "$here/launch-active-run.sh" "$out"
true
