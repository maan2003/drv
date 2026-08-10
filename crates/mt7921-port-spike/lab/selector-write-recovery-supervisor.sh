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

device_path=$(readlink -f "/sys/bus/pci/devices/$bdf")
connected_bssid=
connected_frequency=
for net in /sys/class/net/*; do
  [[ -e $net/device && $(readlink -f "$net/device") == "$device_path" ]] || continue
  link=$(timeout 2 iw dev "$(basename "$net")" link 2>/dev/null) || continue
  bssid=$(awk '/^Connected to / { print $3; exit }' <<< "$link")
  frequency=$(awk '/^[[:space:]]*freq:/ { print $2; exit }' <<< "$link")
  [[ $bssid =~ ^([[:xdigit:]]{2}:){5}[[:xdigit:]]{2}$ && $frequency =~ ^[0-9]+$ ]] \
    || continue
  [[ -z $connected_bssid ]] || {
    echo "multiple connected target Wi-Fi interfaces; refusing handoff" >&2
    exit 1
  }
  connected_bssid=$bssid
  connected_frequency=$frequency
done
[[ -n $connected_bssid ]] || {
  echo "target Wi-Fi interface is not connected; refusing handoff" >&2
  exit 1
}
case $connected_frequency in
  5[0-9][0-9][0-9])
    (((connected_frequency - 5000) % 5 == 0)) || exit 1
    connected_channel=$(((connected_frequency - 5000) / 5))
    case $connected_channel in
      36|40|44|48|52|56|60|64|100|104|108|112|116|120|124|128|132|136|140|144|149|153|157|161|165) ;;
      *) exit 1 ;;
    esac
    ;;
  *)
    echo "connected target is outside the source-exact 5 GHz SAE boundary" >&2
    exit 1
    ;;
esac
export DRV_SAE_BSSID=$connected_bssid DRV_SAE_CHANNEL=$connected_channel
token=$(wifi-lab-watchdog arm) || exit 1
printf 'START realtime=%s bdf=%s\n' "$start" "$bdf" >> "$timeline"
printf 'TARGET bssid=%s channel=%s frequency=%s\n' \
  "$connected_bssid" "$connected_channel" "$connected_frequency" >> "$timeline"
sync -f "$timeline"

wifi-driver-lab "$bdf" 300 -- "$@"
experiment_rc=$?
restore_ns=$(date +%s%N)
printf 'RESTORE_RETURN realtime=%s rc=%s\n' "$(date --iso-8601=ns)" "$experiment_rc" >> "$timeline"
sync -f "$timeline"

wiphy_ready=false
interface_ready=false
association=false
dhcp=false
default_route=false
connectivity=false
association_failure=false
connectivity_ms=-1
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

  if ! $wiphy_ready; then
    for phy in /sys/class/ieee80211/*; do
      [[ -e $phy ]] || continue
      if [[ $(readlink -f "$phy/device") == "$device_path" ]]; then
        wiphy_ready=true
        printf 'TRANSITION wiphy_ready realtime=%s phy=%s\n' \
          "$now" "$(basename "$phy")" >> "$timeline"
        break
      fi
    done
  fi

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
    station=$(timeout 2 iwctl station "$name" show 2>&1)
    station_rc=$?
    printf '%s' "$station" | head -c 2048 | sed 's/^/IWD_STATION /' >> "$timeline" || true
    printf '\n' >> "$timeline"
    if ! $interface_ready && [[ $(readlink -f "$net/device") == "$device_path" ]] \
      && ((station_rc == 0)); then
      interface_ready=true
      printf 'TRANSITION usable_interface_ready realtime=%s interface=%s\n' \
        "$now" "$name" >> "$timeline"
    fi
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
    connectivity_ms=$((($(date +%s%N) - restore_ns) / 1000000))
    printf 'TRANSITION connectivity realtime=%s interface=%s gateway=%s restore_elapsed_ms=%s\n' \
      "$now" "$route_if" "$gateway" "$connectivity_ms" >> "$timeline"
  fi
  sync -f "$timeline"
  sync -f "$messages" 2>/dev/null || true
  if ! $association_failure && grep -Eq 'association-timeout|connect-failed' "$messages"; then
    association_failure=true
    printf 'TRANSITION association_failure realtime=%s evidence=association-timeout_or_connect-failed\n' \
      "$now" >> "$timeline"
    sync -f "$timeline"
  fi

  shopt -s nullglob
  states=(/run/wifi-driver-lab/*.state)
  safety=(/run/wifi-driver-lab/*.state.safety)
  unsafe=false
  for file in "${safety[@]}"; do
    [[ $(<"$file") == SAFE ]] || unsafe=true
  done
  if ((${#states[@]} == 0)) && ! $unsafe \
    && [[ $driver == mt7921e && $power == D0 && $iwd_active == active ]] \
    && $association && $dhcp && $default_route && $connectivity; then
    wifi-lab-watchdog disarm "$token"
    outcome=passed
    reason=none
    if ((experiment_rc != 0)); then
      outcome=failed
      reason=experiment_rc_$experiment_rc
    elif $association_failure; then
      outcome=failed
      reason=association_failure
    elif ((connectivity_ms > 60000)); then
      outcome=failed
      reason=restore_to_connectivity_over_60s
    fi
    printf 'COMPLETE realtime=%s watchdog=disarmed outcome=%s reason=%s restore_elapsed_ms=%s\n' \
      "$(date --iso-8601=ns)" "$outcome" "$reason" "$connectivity_ms" >> "$timeline"
    sync -f "$timeline"
    [[ $outcome == passed ]]
    exit $?
  fi
  sleep 2
done

printf 'INCOMPLETE realtime=%s wiphy_ready=%s usable_interface_ready=%s association=%s ipv4=%s default_route=%s connectivity=%s association_failure=%s watchdog=armed\n' \
  "$(date --iso-8601=ns)" "$wiphy_ready" "$interface_ready" "$association" "$dhcp" \
  "$default_route" "$connectivity" "$association_failure" >> "$timeline"
sync -f "$timeline"
exit "$experiment_rc"
