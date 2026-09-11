#!/usr/bin/env bash
set -u

export PATH=@runtime_path@
wifi_driver_lab=@wifi_driver_lab@
wifi_lab_watchdog=@wifi_lab_watchdog@
validation_launcher=@validation_launcher@
artifact_identity=@artifact_identity@
recovery_deadline_ms=@recovery_deadline_ms@
sys_root=@sys_root@
run_root=@run_root@
var_root=@var_root@
id_command=@id_command@
native_client_mac=@native_client_mac@
session_client_mac=@session_client_mac@
identity_mode=@identity_mode@
hardware_lock=@hardware_lock@
umask 077

plan=false
if [[ ${1-} == --plan ]]; then
  plan=true
  shift
fi
bdf=${1:?PCI BDF is required}
shift
[[ ${1-} == -- ]] || exit 2
shift
(($# == 1)) || {
  echo "fixed validation supervisor requires its sole packaged launcher with no launcher arguments" >&2
  exit 2
}
[[ $1 == "$validation_launcher" ]] || {
  echo "fixed validation supervisor refuses arbitrary launchers" >&2
  exit 2
}
[[ $($validation_launcher --artifact-identity) == "$(cat "$artifact_identity")" ]] || {
  echo "fixed validation supervisor rejects launcher semantic identity" >&2
  exit 78
}
[[ $bdf =~ ^[[:xdigit:]]{4}:[[:xdigit:]]{2}:[[:xdigit:]]{2}[.][[:xdigit:]]$ ]] || exit 2
root=$var_root/lib/wifi-driver-lab

[[ $($id_command -u) == 0 ]] || {
  echo "fixed validation supervisor requires noninteractive root elevation before any state change" >&2
  exit 77
}

if $plan; then
  [[ $validation_launcher == /nix/store/* && -x $validation_launcher ]] || {
    echo "fixed validation launcher is missing or outside the Nix store" >&2
    exit 2
  }
  [[ -d $root && -w $root ]] || {
    echo "durable report directory is not writable by root" >&2
    exit 77
  }
  printf 'PLAN mode=inert hardware_handoff=false identity_mode=%s native_identity_restore_required=true uid=0 privilege_contract=sudo_-n durable_report_dir=%s durable_report_writable=true supervisor=%s supervisor_sha256=%s wifi_driver_lab=%s wifi_driver_lab_sha256=%s wifi_lab_watchdog=%s wifi_lab_watchdog_sha256=%s bdf=%s timeout_seconds=420 watchdog_owner=selector-write-recovery-supervisor_external_arm_heartbeat_recovery_exact_token_disarm launcher=%s launcher_sha256=%s argv=' \
    "$identity_mode" \
    "$root" \
    "$(readlink -f "$0")" "$(sha256sum "$(readlink -f "$0")" | cut -d ' ' -f1)" \
    "$wifi_driver_lab" "$(sha256sum "$wifi_driver_lab" | cut -d ' ' -f1)" \
    "$wifi_lab_watchdog" "$(sha256sum "$wifi_lab_watchdog" | cut -d ' ' -f1)" \
    "$bdf" "$validation_launcher" "$(sha256sum "$validation_launcher" | cut -d ' ' -f1)"
  printf ' %q' "$validation_launcher"
  printf '\n'
  exit 0
fi

# Serialize the complete destructive handoff and recovery lifecycle with every
# other physical-hardware workflow.  Keep fd 8 open until the supervisor exits.
exec 8>"$hardware_lock"
if ! @flock@ -n 8; then
  echo "physical hardware is already owned by another workflow" >&2
  exit 75
fi
if ! "$wifi_driver_lab" --idle; then
  echo "wifi-driver-lab has unresolved state; refusing handoff" >&2
  exit 75
fi
"$wifi_driver_lab" --quarantined
quarantined_rc=$?
if ((quarantined_rc != 1)); then
  echo "wifi-driver-lab quarantine status is not clear; refusing handoff" >&2
  exit 75
fi
if ! "$wifi_driver_lab" --native-ready "$bdf"; then
  echo "native Wi-Fi readiness check failed; refusing handoff" >&2
  exit 75
fi

normalize_iw_frequency() {
  local frequency=$1
  [[ $frequency =~ ^[0-9]+([.]0)?$ ]] || return 1
  printf '%s\n' "${frequency%.0}"
}

monotonic_ms() {
  awk '{ printf "%.0f\n", $1 * 1000 }' /proc/uptime
}

reconnect_due() {
  local elapsed_ms=$1 eligible_since_ms=$2 attempts=$3 last_attempt_ms=$4
  ((elapsed_ms <= 55000 && eligible_since_ms >= 0 && attempts < 2)) || return 1
  ((elapsed_ms - eligible_since_ms >= 15000)) || return 1
  ((last_attempt_ms < 0 || elapsed_ms - last_attempt_ms >= 30000))
}

native_reconnect_eligible() {
  local interface=$1 station station_state file net candidate matches=0
  [[ -n $interface \
    && $(basename "$(readlink -f "$sys_root/bus/pci/devices/$bdf/driver")") == mt7921e \
    && $(cat "$sys_root/bus/pci/devices/$bdf/power_state" 2>/dev/null) == D0 \
    && $(systemctl is-active iwd.service 2>/dev/null) == active ]] || return 1
  for net in "$sys_root"/class/net/wlan*; do
    [[ -e $net && $(readlink -f "$net/device") == "$device_path" \
      && $(cat "$net/address" 2>/dev/null) == "$native_client_mac" ]] || continue
    candidate=$(basename "$net")
    timeout 2 iwctl station "$candidate" show >/dev/null 2>&1 || continue
    matches=$((matches + 1))
    [[ $candidate == "$interface" ]] || return 1
  done
  ((matches == 1)) || return 1
  station=$(timeout 2 iwctl station "$interface" show 2>/dev/null) || return 1
  station_state=$(printf '%s\n' "$station" | sed 's/\x1b\[[0-9;]*m//g' \
    | awk '$1 == "State" { print $2; exit }')
  [[ $station_state == disconnected ]] || return 1
  shopt -s nullglob
  local states=("$run_root"/wifi-driver-lab/*.state)
  local safety=("$run_root"/wifi-driver-lab/*.state.safety)
  ((${#states[@]} == 0)) || return 1
  for file in "${safety[@]}"; do
    [[ $(<"$file") == SAFE ]] || return 1
  done
}

scan_for_native_network() {
  local interface=$1 started now networks
  timeout 3 iwctl station "$interface" scan >/dev/null 2>&1 || return 1
  started=$(monotonic_ms)
  while true; do
    networks=$(timeout 1 iwctl station "$interface" get-networks 2>/dev/null || true)
    if printf '%s\n' "$networks" | sed 's/\x1b\[[0-9;]*m//g' \
      | grep -qE '[[:space:]]ajay[[:space:]]'; then
      return 0
    fi
    now=$(monotonic_ms)
    ((now - started < 7000)) || return 1
    sleep 1
  done
}

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

device_path=$(readlink -f "$sys_root/bus/pci/devices/$bdf")
connected_bssid=
connected_frequency=
connected_ssid=
connected_client_mac=
for net in "$sys_root"/class/net/*; do
  [[ -e $net/device && $(readlink -f "$net/device") == "$device_path" ]] || continue
  link=$(timeout 2 iw dev "$(basename "$net")" link 2>/dev/null) || continue
  bssid=$(awk '/^Connected to / { print $3; exit }' <<< "$link")
  frequency=$(awk '/^[[:space:]]*freq:/ { print $2; exit }' <<< "$link")
  ssid=$(awk '/^[[:space:]]*SSID:/ { sub(/^[[:space:]]*SSID:[[:space:]]*/, ""); print; exit }' <<< "$link")
  [[ $bssid =~ ^([[:xdigit:]]{2}:){5}[[:xdigit:]]{2}$ ]] || continue
  frequency=$(normalize_iw_frequency "$frequency") || continue
  client_mac=$(cat "$net/address" 2>/dev/null) || continue
  [[ $client_mac =~ ^([[:xdigit:]]{2}:){5}[[:xdigit:]]{2}$ ]] || continue
  first_octet=${client_mac%%:*}
  (( (16#$first_octet & 3) == 2 )) || {
    echo "connected target does not use a local unicast VIF address" >&2
    exit 1
  }
  [[ -z $connected_bssid ]] || {
    echo "multiple connected target Wi-Fi interfaces; refusing handoff" >&2
    exit 1
  }
  connected_bssid=$bssid
  connected_frequency=$frequency
  connected_ssid=$ssid
  connected_client_mac=$client_mac
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
if [[ $connected_ssid != ajay || $connected_client_mac != "$native_client_mac" ]]; then
  echo "connected Wi-Fi target does not match packaged ajay/native-identity policy" >&2
  exit 1
fi
export DRV_SAE_BSSID=$connected_bssid DRV_SAE_CHANNEL=$connected_channel \
  DRV_SAE_CLIENT_MAC=$session_client_mac
token=$("$wifi_lab_watchdog" arm) || exit 1
printf 'START realtime=%s bdf=%s\n' "$start" "$bdf" >> "$timeline"
printf 'TARGET bssid=%s channel=%s frequency=%s native_client_mac=%s session_client_mac=%s identity_mode=%s\n' \
  "$connected_bssid" "$connected_channel" "$connected_frequency" \
  "$connected_client_mac" "$session_client_mac" "$identity_mode" >> "$timeline"
sync -f "$timeline"

"$wifi_driver_lab" "$bdf" 420 -- "$@" &
experiment_pid=$!
while kill -0 "$experiment_pid" 2>/dev/null; do
  if ! "$wifi_lab_watchdog" heartbeat "$token"; then
    echo "watchdog heartbeat failed; recovery remains reboot-owned" >&2
  fi
  sleep 5
done
wait "$experiment_pid"
experiment_rc=$?
restore_ns=$(date +%s%N)
restore_mono_ms=$(monotonic_ms)
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
sample=0
eligible_since_ms=-1
reconnect_attempts=0
last_reconnect_ms=-1
while true; do
  elapsed_ms=$(($(monotonic_ms) - restore_mono_ms))
  ((elapsed_ms < recovery_deadline_ms)) || break
  now=$(date --iso-8601=ns)
  driver=none
  [[ -L $sys_root/bus/pci/devices/$bdf/driver ]] && \
    driver=$(basename "$(readlink -f "$sys_root/bus/pci/devices/$bdf/driver")")
  power=$(cat "$sys_root/bus/pci/devices/$bdf/power_state" 2>/dev/null || printf unknown)
  runtime=$(cat "$sys_root/bus/pci/devices/$bdf/power/runtime_status" 2>/dev/null || printf unknown)
  iwd_active=$(systemctl is-active iwd.service 2>/dev/null || true)
  iwd_sub=$(systemctl show iwd.service --property=SubState --value 2>/dev/null || true)
  printf 'SAMPLE n=%s realtime=%s driver=%s power=%s runtime=%s iwd=%s/%s\n' \
    "$sample" "$now" "$driver" "$power" "$runtime" "$iwd_active" "$iwd_sub" >> "$timeline"

  if ! $wiphy_ready; then
    for phy in "$sys_root"/class/ieee80211/*; do
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
  native_identity_restored=false
  native_if=""
  native_station_state=""
  native_interface_ambiguous=false
  connectivity_now=false
  for net in "$sys_root"/class/net/wlan*; do
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
    if [[ $(readlink -f "$net/device") == "$device_path" && $address == "$native_client_mac" \
      && $station_rc == 0 ]]; then
      if [[ -n $native_if && $native_if != "$name" ]]; then
        native_interface_ambiguous=true
      else
        native_if=$name
        native_station_state=$(printf '%s\n' "$station" | sed 's/\x1b\[[0-9;]*m//g' \
          | awk '$1 == "State" { print $2; exit }')
      fi
      link=$(timeout 2 iw dev "$name" link 2>/dev/null || true)
      linked_ssid=$(awk '/^[[:space:]]*SSID:/ { sub(/^[[:space:]]*SSID:[[:space:]]*/, ""); print; exit }' <<< "$link")
      [[ $carrier == 1 && $linked_ssid == ajay ]] && associated_if=$name
    fi
    if [[ $(readlink -f "$net/device") == "$device_path" && $address == "$native_client_mac" ]] \
      && ip -4 -o address show dev "$name" scope global | grep -q .; then
      ipv4_if=$name
    fi
  done
  if [[ -n $native_if ]] && ! $native_interface_ambiguous; then
    native_identity_restored=true
  else
    native_if=""
    associated_if=""
  fi
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
  route_if=""
  if [[ -n $associated_if ]] && ip route show default dev "$associated_if" | grep -q .; then
    route_if=$associated_if
  fi
  if ! $default_route && [[ -n $route_if ]]; then
    default_route=true
    printf 'TRANSITION default_route realtime=%s interface=%s\n' "$now" "$route_if" >> "$timeline"
  fi
  gateway=""
  [[ -z $route_if ]] || gateway=$(ip route show default dev "$route_if" | awk '{ print $3; exit }')
  if [[ -n $associated_if && -n $ipv4_if && -n $route_if && -n $gateway ]] \
    && ping -I "$native_if" -c 1 -W 1 "$gateway" >/dev/null 2>&1; then
    connectivity_now=true
    if ! $connectivity; then
      connectivity=true
      connectivity_ms=$((($(date +%s%N) - restore_ns) / 1000000))
      printf 'TRANSITION connectivity realtime=%s interface=%s gateway=%s restore_elapsed_ms=%s\n' \
        "$now" "$route_if" "$gateway" "$connectivity_ms" >> "$timeline"
    fi
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
  states=("$run_root"/wifi-driver-lab/*.state)
  safety=("$run_root"/wifi-driver-lab/*.state.safety)
  unsafe=false
  for file in "${safety[@]}"; do
    [[ $(<"$file") == SAFE ]] || unsafe=true
  done

  reconnect_eligible=false
  if ((${#states[@]} == 0)) && ! $unsafe && [[ -n $native_if \
    && $driver == mt7921e && $power == D0 && $iwd_active == active ]]; then
    reconnect_eligible=true
    ((eligible_since_ms >= 0)) || eligible_since_ms=$elapsed_ms
  else
    eligible_since_ms=-1
  fi
  elapsed_ms=$(($(monotonic_ms) - restore_mono_ms))
  if $reconnect_eligible && [[ $native_station_state == disconnected ]] \
    && reconnect_due "$elapsed_ms" "$eligible_since_ms" "$reconnect_attempts" "$last_reconnect_ms"; then
    reconnect_attempts=$((reconnect_attempts + 1))
    last_reconnect_ms=$elapsed_ms
    printf 'RECOVERY_RECONNECT attempt=%s realtime=%s interface=%s phase=scan\n' \
      "$reconnect_attempts" "$now" "$native_if" >> "$timeline"
    if scan_for_native_network "$native_if" && native_reconnect_eligible "$native_if" \
      && (($(monotonic_ms) - restore_mono_ms <= 55000)); then
      timeout 15 iwctl station "$native_if" connect ajay </dev/null >> "$timeline" 2>&1
      reconnect_rc=$?
      printf 'RECOVERY_RECONNECT attempt=%s realtime=%s interface=%s phase=connect rc=%s\n' \
        "$reconnect_attempts" "$(date --iso-8601=ns)" "$native_if" "$reconnect_rc" >> "$timeline"
    else
      printf 'RECOVERY_RECONNECT attempt=%s realtime=%s interface=%s phase=scan_or_revalidation_failed\n' \
        "$reconnect_attempts" "$(date --iso-8601=ns)" "$native_if" >> "$timeline"
    fi
    sync -f "$timeline"
  fi
  if ((${#states[@]} == 0)) && ! $unsafe \
    && [[ $driver == mt7921e && $power == D0 && $iwd_active == active ]] \
    && $native_identity_restored && [[ $associated_if == "$native_if" \
      && $ipv4_if == "$native_if" && $route_if == "$native_if" && -n $gateway ]] \
    && $connectivity_now; then
    "$wifi_lab_watchdog" disarm "$token"
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
    ((experiment_rc != 0)) && exit "$experiment_rc"
    [[ $outcome == passed ]]
    exit $?
  fi
  sample=$((sample + 1))
  sleep 2
done

printf 'INCOMPLETE realtime=%s wiphy_ready=%s usable_interface_ready=%s native_identity_restored=%s association=%s ipv4=%s default_route=%s connectivity=%s association_failure=%s watchdog=armed\n' \
  "$(date --iso-8601=ns)" "$wiphy_ready" "$interface_ready" "$native_identity_restored" \
  "$association" "$dhcp" "$default_route" "$connectivity" "$association_failure" >> "$timeline"
sync -f "$timeline"
((experiment_rc != 0)) && exit "$experiment_rc"
exit 75
