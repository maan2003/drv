#!@shell@
set -euo pipefail

contract=mt7921-full-firmware-recovery-status-v1
run_dir=@run_root@/wifi-driver-lab

if [ "$(@id@ -u)" -ne 0 ]; then
  echo "$contract requires root for authoritative state reads" >&2
  exit 77
fi

case "$#:${1-}" in
  1:--version)
    printf '%s\n' "$contract"
    ;;
  1:--idle)
    shopt -s nullglob
    states=("$run_dir"/*.state)
    [ "${#states[@]}" -eq 0 ]
    ;;
  1:--quarantined)
    [ -d "$run_dir" ] && [ -r "$run_dir" ] && [ -x "$run_dir" ] || exit 2
    shopt -s nullglob
    for safety in "$run_dir"/*.state.safety; do
      safety_state=$(@cat@ -- "$safety") || exit 2
      [ "$safety_state" = SAFE ] || exit 0
    done
    [ -d "$run_dir" ] && [ -r "$run_dir" ] && [ -x "$run_dir" ] || exit 2
    printf 'clear\n'
    exit 1
    ;;
  2:--native-ready)
    bdf=$2
    [ "$bdf" = 0000:05:00.0 ] || exit 64
    device=@sys_root@/bus/pci/devices/$bdf
    [ -L "$device/driver" ] || exit 1
    [ "$(@basename@ "$(@readlink@ -f "$device/driver")")" = mt7921e ] || exit 1
    @systemctl@ is-active --quiet iwd.service || exit 1
    shopt -s nullglob
    netdevs=("$device"/net/*)
    [ "${#netdevs[@]}" -eq 1 ] || exit 1
    net=$(@basename@ "${netdevs[0]}")
    @ip@ -4 addr show dev "$net" | @grep@ -q 'inet ' || exit 1
    gateway=$(@ip@ route show default dev "$net" | @awk@ 'NR==1 {print $3}')
    [ -n "$gateway" ] || exit 1
    @ping@ -I "$net" -c 1 -W 2 "$gateway" >/dev/null
    ;;
  *)
    echo "usage: mt7921-full-firmware-validation-recovery-status --version|--idle|--quarantined|--native-ready 0000:05:00.0" >&2
    exit 64
    ;;
esac
