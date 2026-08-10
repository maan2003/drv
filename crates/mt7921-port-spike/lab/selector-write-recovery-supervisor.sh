#!/run/current-system/sw/bin/bash
set -u

export PATH=/run/current-system/sw/bin
umask 077

bdf=${1:?PCI BDF is required}
shift
[[ ${1-} == -- ]] || exit 2
shift
(($# > 0)) || exit 2

root=/var/lib/wifi-driver-lab
stamp=$(date --utc +%Y%m%dT%H%M%SZ)
timeline=$root/selector-write-recovery-$stamp.log
messages=$root/selector-write-recovery-$stamp.messages.log
start=$(date --iso-8601=ns)
journal_since=$(date --iso-8601=seconds)
install -d -m 0700 "$root"
: > "$timeline"
: > "$messages"

journalctl --follow --since "$journal_since" --output=short-precise \
  _TRANSPORT=kernel + _SYSTEMD_UNIT=iwd.service > "$messages" &
journal_pid=$!
cleanup() {
  kill "$journal_pid" 2>/dev/null || true
  wait "$journal_pid" 2>/dev/null || true
  sync -f "$messages" 2>/dev/null || true
}
trap cleanup EXIT

token=$(wifi-lab-watchdog arm) || exit 1
printf 'START realtime=%s bdf=%s\n' "$start" "$bdf" >> "$timeline"
sync -f "$timeline"

wifi-driver-lab "$bdf" 300 -- "$@"
experiment_rc=$?
printf 'RESTORE_RETURN realtime=%s rc=%s\n' "$(date --iso-8601=ns)" "$experiment_rc" >> "$timeline"
sync -f "$timeline"

association=false
dhcp=false
default_route=false
connectivity=false
for sample in $(seq 0 54); do
  now=$(date --iso-8601=ns)
  driver=none
  [[ -L /sys/bus/pci/devices/$bdf/driver ]] && \
    driver=$(basename "$(readlink -f "/sys/bus/pci/devices/$bdf/driver")")
  power=$(cat "/sys/bus/pci/devices/$bdf/power_state" 2>/dev/null || printf unknown)
  runtime=$(cat "/sys/bus/pci/devices/$bdf/power/runtime_status" 2>/dev/null || printf unknown)
  iwd_active=$(systemctl is-active iwd.service 2>/dev/null || true)
  iwd_sub=$(systemctl show iwd.service --property=SubState --value 2>/dev/null || true)
  printf 'SAMPLE n=%s realtime=%s driver=%s power=%s runtime=%s iwd=%s/%s\n' \
    "$sample" "$now" "$driver" "$power" "$runtime" "$iwd_active" "$iwd_sub" >> "$timeline"

  associated_if=""
  ipv4_if=""
  for net in /sys/class/net/wlan*; do
    [[ -e $net ]] || continue
    name=$(basename "$net")
    operstate=$(cat "$net/operstate" 2>/dev/null || printf unknown)
    address=$(cat "$net/address" 2>/dev/null || printf unknown)
    carrier=$(cat "$net/carrier" 2>/dev/null || printf 0)
    printf 'WLAN name=%s operstate=%s carrier=%s address=%s\n' \
      "$name" "$operstate" "$carrier" "$address" >> "$timeline"
    timeout 2 iwctl station "$name" show 2>&1 | head -c 2048 | sed 's/^/IWD_STATION /' >> "$timeline" || true
    printf '\n' >> "$timeline"
    [[ $carrier == 1 ]] && associated_if=$name
    if ip -4 -o address show dev "$name" scope global | grep -q .; then
      ipv4_if=$name
    fi
  done
  ip -brief address show | sed 's/^/ADDRESS /' >> "$timeline"
  ip route show | sed 's/^/ROUTE /' >> "$timeline"

  if ! $association && [[ -n $associated_if ]]; then
    association=true
    printf 'TRANSITION association realtime=%s interface=%s\n' "$now" "$associated_if" >> "$timeline"
  fi
  if ! $dhcp && [[ -n $ipv4_if ]]; then
    dhcp=true
    printf 'TRANSITION ipv4 realtime=%s interface=%s\n' "$now" "$ipv4_if" >> "$timeline"
  fi
  route_if=$(ip route show default | awk '/ dev wlan/ { for (i = 1; i <= NF; i++) if ($i == "dev") { print $(i + 1); exit } }')
  if ! $default_route && [[ -n $route_if ]]; then
    default_route=true
    printf 'TRANSITION default_route realtime=%s interface=%s\n' "$now" "$route_if" >> "$timeline"
  fi
  gateway=$(ip route show default | awk '/ dev wlan/ { print $3; exit }')
  if ! $connectivity && [[ -n $associated_if && -n $ipv4_if && -n $route_if && -n $gateway ]] \
    && ping -c 1 -W 1 "$gateway" >/dev/null 2>&1; then
    connectivity=true
    printf 'TRANSITION connectivity realtime=%s interface=%s gateway=%s\n' \
      "$now" "$route_if" "$gateway" >> "$timeline"
  fi
  sync -f "$timeline"
  sync -f "$messages" 2>/dev/null || true

  shopt -s nullglob
  states=(/run/wifi-driver-lab/*.state)
  safety=(/run/wifi-driver-lab/*.state.safety)
  unsafe=false
  for file in "${safety[@]}"; do
    [[ $(<"$file") == SAFE ]] || unsafe=true
  done
  if ((experiment_rc == 0 && ${#states[@]} == 0)) && ! $unsafe \
    && [[ $driver == mt7921e && $power == D0 && $iwd_active == active ]] \
    && $association && $dhcp && $default_route && $connectivity; then
    wifi-lab-watchdog disarm "$token"
    printf 'COMPLETE realtime=%s watchdog=disarmed\n' "$(date --iso-8601=ns)" >> "$timeline"
    sync -f "$timeline"
    exit 0
  fi
  sleep 2
done

printf 'INCOMPLETE realtime=%s association=%s ipv4=%s default_route=%s connectivity=%s watchdog=armed\n' \
  "$(date --iso-8601=ns)" "$association" "$dhcp" "$default_route" "$connectivity" >> "$timeline"
sync -f "$timeline"
exit "$experiment_rc"
