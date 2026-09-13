#!/usr/bin/env bash
# ROOT must already contain tailscaled, provider, virtio frame adapter and ELF/CA files.
# Uses an unprivileged QEMU/SLIRP gateway; never changes host interfaces or Tailscale.
set -euo pipefail
umask 077
kernel=${1:?usage: run-tailscale-kvm.sh KERNEL ROOT NEW_OUTPUT_DIRECTORY}
root=${2:?}
out=${3:?}
qemu=${QEMU:-qemu-system-x86_64}
mkdir -m700 "$out"
out=$(cd "$out"; pwd)
cp "$(dirname "$0")/tailscale-guest-init" "$root/init"
chmod +x "$root/init"
(cd "$root"; find . -print0 | cpio --null -o -H newc) | gzip -1 >"$out/initrd.gz"
"$qemu" -machine none -nodefaults -display none -monitor none -serial none \
    -netdev user,id=wan,net=10.0.2.0/24 \
    -netdev stream,id=wire,server=on,addr.type=unix,addr.path="$out/ethernet.sock" \
    -netdev hubport,id=uplink,hubid=0,netdev=wan \
    -netdev hubport,id=port,hubid=0,netdev=wire \
    -daemonize -pidfile "$out/gateway.pid" 2>"$out/gateway.log"
trap 'kill "$(cat "$out/gateway.pid")" 2>/dev/null || true' ERR
"$qemu" -enable-kvm -machine q35,kernel-irqchip=split -cpu host -smp 2 -m 1024 \
    -nodefaults -no-reboot -display none -monitor none -serial file:"$out/serial.log" \
    -device intel-iommu,intremap=on,caching-mode=on -device virtio-serial-pci \
    -chardev socket,id=frames,path="$out/ethernet.sock" \
    -device virtserialport,chardev=frames,name=drv.ethernet \
    -chardev socket,id=control,path="$out/control.sock",server=on,wait=off \
    -device virtserialport,chardev=control,name=drv.control \
    -kernel "$kernel" -initrd "$out/initrd.gz" \
    -append 'console=ttyS0 panic=-1 rdinit=/init intel_iommu=on iommu.strict=1' \
    -daemonize -pidfile "$out/guest.pid"
echo "Private login/control logs: $out"
# Bound the lab independently of guest progress, and clean up both private processes.
nohup bash -c '
    sleep 3600
    for file in "$1/guest.pid" "$1/gateway.pid"; do
        pid=$(cat "$file")
        # Avoid signaling an unrelated process if a pid has been reused.
        if test -r "/proc/$pid/cmdline" && tr "\\0" " " <"/proc/$pid/cmdline" | grep -Fq -- "$1"; then
            kill "$pid" 2>/dev/null || true
        fi
    done
' bash "$out" >/dev/null 2>&1 </dev/null &
echo "$!" >"$out/cleanup.pid"
echo 'Private guest and gateway are limited to one hour.' 
