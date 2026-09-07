#!/usr/bin/env bash
# Launch one active-client run on np with the given fast-build/nix store path.
#
# Target is ajay (the phone hotspot), which does its own NAT to the internet,
# so a run needs only np + the phone. No redwood AP, firewall, NAT, or hostapd
# deauth guard is involved (that machinery existed only for the ph1/redwood AP).
set -u
store="${1:?store path}"
here=$(dirname "$0")
e=$(date -u +%s)
"$here/np-ssh.sh" "(nohup setsid /data/persist/drvlab/regen-and-run.sh $store > /data/persist/drvlab/regen-run-$e.log 2>&1 < /dev/null &); sleep 2; cat /data/persist/drvlab/regen-run-$e.log"
echo "launch-epoch=$e log=/data/persist/drvlab/regen-run-$e.log"
