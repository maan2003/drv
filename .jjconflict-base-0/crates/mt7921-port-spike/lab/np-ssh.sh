#!/usr/bin/env bash
# ssh to no-plastic over whichever path is currently alive:
#  1. tailscale (Host np in ~/.ssh/lab.conf) when np is on ajay/internet
#  2. redwood jump to np's ph1 LAN address when np fell back to ph1
# usage: np-ssh.sh [-t timeout] 'remote command'
set -u
cfg=$HOME/.ssh/lab.conf
lan=${NP_LAN:-10.77.0.20}
to=8
[ "${1:-}" = -t ] && { to=$2; shift 2; }
if ssh -n -F "$cfg" -o ConnectTimeout=$to np true 2>/dev/null; then  # -n: keep stdin for the real command
  exec ssh -F "$cfg" -o ConnectTimeout=$to np "$@"
fi
exec ssh -F "$cfg" -J redwood -i "$HOME/.ssh/no-plastic" -o IdentitiesOnly=yes -o ConnectTimeout=$to \
  -o StrictHostKeyChecking=accept-new "user@$lan" "$@"
