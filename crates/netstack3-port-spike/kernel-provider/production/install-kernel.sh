#!/usr/bin/env bash
# Install into a disposable Linux 6.18.40 source tree, never the host kernel.
set -euo pipefail
tree=${1:?usage: install-kernel.sh LINUX_SOURCE}
here=$(cd "$(dirname "$0")" && pwd)
test -f "$tree/net/Kconfig"
mkdir -p "$tree/net/netstack3"
cp "$here"/{Kconfig,Makefile,protocol.h,af_netstack3.c} "$tree/net/netstack3/"
grep -qF 'source "net/netstack3/Kconfig"' "$tree/net/Kconfig" ||
    printf '\nsource "net/netstack3/Kconfig"\n' >> "$tree/net/Kconfig"
grep -qF 'obj-$(CONFIG_NETSTACK3) += netstack3/' "$tree/net/Makefile" ||
    printf '\nobj-$(CONFIG_NETSTACK3) += netstack3/\n' >> "$tree/net/Makefile"
