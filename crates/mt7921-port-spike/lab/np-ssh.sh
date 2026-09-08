#!/usr/bin/env bash
# ssh to no-plastic over its ajay/Tailscale path.
# usage: np-ssh.sh [-t timeout] 'remote command'
set -u
cfg=$HOME/.ssh/lab.conf
to=8
[ "${1:-}" = -t ] && { to=$2; shift 2; }
exec ssh -F "$cfg" -o ConnectTimeout=$to np "$@"
