#!/usr/bin/env bash
# Run only as the command of wifi-driver-lab, under an armed host watchdog.
# FD3/FD4 are the existing root launcher credential/regulatory input contract.
set -euo pipefail
umask 077
kernel=${1:?usage: run-wifi-kvm.sh BZIMAGE ROOT NEW_OUTPUT_DIRECTORY}
root=${2:?}
out=${3:?}
: "${DRV_PCI_BDF:?wifi-driver-lab must own the host PCI device}"
: "${DRV_LAB_SAFETY_STATE:?wifi-driver-lab safety ledger required}"
: "${DRV_SAE_BSSID:?supply the current scanned channel-149 ajay BSSID}"
test "$EUID" = 0
test "$(cat "$DRV_LAB_SAFETY_STATE")" = SAFE
test "$(basename "$(readlink "/sys/bus/pci/devices/$DRV_PCI_BDF/driver")")" = vfio-pci
for fd in 3 4; do
    test "$(stat -Lc '%u:%a' "/proc/$$/fd/$fd")" = 0:600
    test -f "/proc/$$/fd/$fd"
done
credential_len=$(wc -c <"/proc/$$/fd/3")
snapshot_len=$(wc -c <"/proc/$$/fd/4")
test "$credential_len" -ge 8 && test "$credential_len" -le 63
test "$snapshot_len" -ge 1 && test "$snapshot_len" -le 4096
for binary in mt7921-passive-scan wlan-stack-kvm wlancfg-service wlanctl netstack3-provider drv-dns-service dns-check loopback-test curl; do
    test -x "$root/bin/$binary"
done
test -f "$root/etc/quad9.toml"
test -f "$root/etc/ssl/certs/ca-certificates.crt"
# drv-dns-service uses a Nix RUNPATH for Rust unwinding. Require the exact
# loader-resolved path inside the guest root; a same-basename fallback in /lib
# does not satisfy that contract.
dns_libgcc=$(ldd "$root/bin/drv-dns-service" |
    awk '$1 == "libgcc_s.so.1" && $2 == "=>" && $3 ~ "^/" { print $3; exit }')
case "$dns_libgcc" in
    /nix/store/*/lib/libgcc_s.so.1) ;;
    *) exit 1 ;;
esac
test -f "$root$dns_libgcc"
test -f "$root/lib/libnss_drv.so.2"
for dependency in $(ldd "$root/lib/libnss_drv.so.2" |
    awk '$2 == "=>" && $3 ~ "^/" { print $3 } $1 ~ "^/" { print $1 }'); do
    test -f "$root$dependency"
done
test "$(stat -Lc '%a' "$root/etc/quad9.toml")" = 444
test "$(stat -Lc '%a' "$root/etc/ssl/certs/ca-certificates.crt")" = 444
test "$(wc -c <"$root/etc/quad9.toml")" -le 8192
test "$(wc -c <"$root/etc/ssl/certs/ca-certificates.crt")" -le 2097152
mkdir -m 700 "$out"
out=$(cd "$out" && pwd)
watchdog=/run/current-system/sw/bin/wifi-lab-watchdog
"$watchdog" status >"$out/watchdog"
grep -q '^armed deadline=' "$out/watchdog"
deadline=$(sed -n 's/^armed deadline=//p' "$out/watchdog")
test "$((deadline - $(date +%s)))" -gt 80
cp "$(dirname "$0")/wifi-guest-init" "$root/init"
chmod +x "$root/init"
mkdir -p "$root/run/current-system/sw/bin"
cat >"$root/run/current-system/sw/bin/wifi-lab-watchdog" <<'SH'
#!/bin/busybox sh
# Actual host status captured before launch; VM timeout precedes its deadline.
test "$#" = 1 && test "$1" = status || exit 1
exec /bin/busybox cat /sys/firmware/qemu_fw_cfg/by_name/opt/drv-watchdog/raw
SH
chmod +x "$root/run/current-system/sw/bin/wifi-lab-watchdog"
(cd "$root"; find . -print0 | cpio --null -o -H newc) | gzip -1 >"$out/initrd.gz"
# Recheck after packaging. Never start DMA if the watchdog budget was consumed.
test "$((deadline - $(date +%s)))" -gt 80
printf 'MUTATED\n' >"$DRV_LAB_SAFETY_STATE"
set +e
timeout -k 3 75 "${QEMU:-qemu-system-x86_64}" \
    -enable-kvm -machine q35,kernel-irqchip=split -cpu host -smp 2 -m 1024 \
    -nodefaults -no-reboot -display none -monitor none -serial stdio \
    -device intel-iommu,intremap=on,caching-mode=on \
    -device pcie-root-port,id=wifiport,chassis=1,slot=1 \
    -device "vfio-pci,host=$DRV_PCI_BDF,bus=wifiport,addr=0.0" \
    -kernel "$kernel" -initrd "$out/initrd.gz" \
    -append 'console=ttyS0 panic=-1 rdinit=/init intel_iommu=on iommu.strict=1 vfio_pci.disable_idle_d3=1' \
    -fw_cfg name=opt/drv-credential,file=/proc/self/fd/3 \
    -fw_cfg name=opt/drv-regulatory,file=/proc/self/fd/4 \
    -fw_cfg "name=opt/drv-watchdog,file=$out/watchdog" \
    -fw_cfg "name=opt/drv-bssid,string=$DRV_SAE_BSSID" >"$out/serial.log" 2>&1
rc=$?
set -e
if test "$rc" = 0 && grep -q '^GUEST_MT_HARDWARE_SAFE' "$out/serial.log"; then
    printf 'SAFE\n' >"$DRV_LAB_SAFETY_STATE"
fi
cat "$out/serial.log"
test "$rc" = 0
grep -q '^PASS_MT_GUEST_INTERNET' "$out/serial.log"
