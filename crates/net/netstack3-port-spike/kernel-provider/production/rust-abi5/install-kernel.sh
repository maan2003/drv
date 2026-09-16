#!/usr/bin/env bash
set -euo pipefail
tree=${1:?usage: install-kernel.sh DISPOSABLE_LINUX_7_3_SOURCE}
here=$(cd "$(dirname "$0")" && pwd)
if ! grep -q 'pub unsafe fn register_wait_raw' "$tree/rust/kernel/sync/poll.rs"; then
    patch -d "$tree" -p1 < "$here/../rust-lifecycle/positionless-poll.patch"
fi
# Linux 7.3 moved schedule_work() to the non-deprecated per-CPU queue;
# keep Rust's existing system() helper on that same source of truth.
if grep -q 'Queue::from_raw(bindings::system_wq)' "$tree/rust/kernel/workqueue.rs"; then
    patch -d "$tree" -p1 < "$here/system-workqueue.patch"
fi
if ! grep -q 'ns3_nl_owner' "$tree/net/netlink/af_netlink.c"; then
    patch -d "$tree" -p1 < "$here/netlink-provider.patch"
fi
if ! grep -q 'bool ns3_delegated' "$tree/net/netlink/af_netlink.c"; then
    patch -d "$tree" -p1 < "$here/namespace-lifetime.patch"
fi
mkdir -p "$tree/net/netstack3_rust"
cp "$here"/{Kconfig,Makefile,glue.c,rust_main.rs,linux.rs,frontend.rs,connection.rs,netlink.rs,namespace.rs} "$tree/net/netstack3_rust/"
cp "$here/../rust-lifecycle/endpoint_file.rs" "$tree/net/netstack3_rust/"
grep -qF 'source "net/netstack3_rust/Kconfig"' "$tree/net/Kconfig" ||
 printf '\nsource "net/netstack3_rust/Kconfig"\n' >>"$tree/net/Kconfig"
grep -qF 'obj-$(CONFIG_NETSTACK3_RUST) += netstack3_rust/' "$tree/net/Makefile" ||
 printf '\nobj-$(CONFIG_NETSTACK3_RUST) += netstack3_rust/\n' >>"$tree/net/Makefile"
