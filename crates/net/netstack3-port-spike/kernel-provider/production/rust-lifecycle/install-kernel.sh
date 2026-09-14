#!/usr/bin/env bash
# Install only in a disposable Linux 7.3-rc2 source tree, never the host kernel.
set -euo pipefail
tree=${1:?usage: install-kernel.sh DISPOSABLE_LINUX_SOURCE}
here=$(cd "$(dirname "$0")" && pwd)
test -f "$tree/net/Kconfig"
if ! grep -q 'pub unsafe fn register_wait_raw' "$tree/rust/kernel/sync/poll.rs"; then
    patch -d "$tree" -p1 < "$here/positionless-poll.patch"
fi
mkdir -p "$tree/net/ns3_rust_lifecycle"
cp "$here"/{Kconfig,Makefile,protocol.h,linux_adapter.c,rust_adapter.rs,lifecycle.rs,endpoint_file.rs} "$tree/net/ns3_rust_lifecycle/"
grep -qF 'source "net/ns3_rust_lifecycle/Kconfig"' "$tree/net/Kconfig" ||
    printf '\nsource "net/ns3_rust_lifecycle/Kconfig"\n' >> "$tree/net/Kconfig"
grep -qF 'obj-$(CONFIG_NETSTACK3_RUST_LIFECYCLE) += ns3_rust_lifecycle/' "$tree/net/Makefile" ||
    printf '\nobj-$(CONFIG_NETSTACK3_RUST_LIFECYCLE) += ns3_rust_lifecycle/\n' >> "$tree/net/Makefile"
