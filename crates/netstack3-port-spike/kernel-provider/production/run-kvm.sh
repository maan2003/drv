#!/usr/bin/env bash
# ROOT must already contain busybox, provider, client and ELF dependencies.
set -euo pipefail
kernel=${1:?usage: run-kvm.sh BZIMAGE ROOT OUTPUT_DIRECTORY}
root=${2:?}
out=${3:?}
mkdir -p "$out"
out=$(cd "$out" && pwd)
cp "$(dirname "$0")/guest-init" "$root/init"
chmod +x "$root/init"
(cd "$root"; find . -print0 | cpio --null -o -H newc) | gzip -1 > "$out/initrd.gz"
timeout 50 "${QEMU:-qemu-system-x86_64}" -enable-kvm -cpu host -m 1024 -smp 2 \
    -nodefaults -no-reboot -nographic -serial stdio -kernel "$kernel" \
    -initrd "$out/initrd.gz" -append 'console=ttyS0 panic=-1 rdinit=/init' \
    > "$out/serial.log" 2>&1
grep -q '^PASS_KVM_PRODUCTION_LOOPBACK' "$out/serial.log"
if grep -E 'FAIL|BUG:|Oops:|Kernel panic' "$out/serial.log"; then exit 1; fi
cat "$out/serial.log"
