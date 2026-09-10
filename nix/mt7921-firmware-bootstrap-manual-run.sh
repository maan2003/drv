#!@shell@
set -eu
umask 077

wifi_driver_lab=@wifi_driver_lab@
driver=@driver@
bdf=0000:05:00.0
run_dir=@run_root@/wifi-driver-lab
report_dir=@var_root@/lib/wifi-driver-lab/reports
wifi_lab_watchdog=@wifi_lab_watchdog@

require_private_directory() {
  directory=$1
  if [ ! -e "$directory" ] && [ ! -L "$directory" ]; then
    @install@ -d -m 0700 "$directory"
  fi
  if [ ! -d "$directory" ] || [ -L "$directory" ] \
    || [ "$(@readlink@ -f -- "$directory")" != "$directory" ]; then
    echo "manual firmware-bootstrap directory is not a canonical directory: $directory" >&2
    exit 73
  fi
  read -r directory_uid directory_mode directory_links <<EOF
$(@stat@ -Lc '%u %a %h' -- "$directory")
EOF
  if [ "$directory_uid" -ne 0 ] || [ "$directory_mode" != 700 ] \
    || [ "$directory_links" -lt 1 ]; then
    echo "manual firmware-bootstrap directory is not root-private: $directory" >&2
    exit 73
  fi
}

if [ "$#" -ne 0 ]; then
  echo "fixed MT7921 manual firmware-bootstrap harness accepts no arguments" >&2
  exit 64
fi
if [ "$(@id@ -u)" -ne 0 ]; then
  echo "MT7921 manual firmware-bootstrap harness must run as root" >&2
  exit 77
fi

require_private_directory "$run_dir"
require_private_directory "@var_root@/lib/wifi-driver-lab"
require_private_directory "$report_dir"
state="$run_dir/manual-firmware-bootstrap-$$-$(@date@ +%s).state"
if [ -e "$state" ] || [ -L "$state" ] || [ -e "$state.safety" ] || [ -L "$state.safety" ]; then
  echo "refusing pre-existing manual firmware-bootstrap state path" >&2
  exit 73
fi
report=$(@mktemp@ "$report_dir/manual-firmware-bootstrap-$(@date@ --utc +%Y%m%dT%H%M%SZ)-XXXXXX.log")
read -r report_uid report_mode report_links <<EOF
$(@stat@ -Lc '%u %a %h' -- "$report")
EOF
if [ ! -f "$report" ] || [ -L "$report" ] || [ "$report_uid" -ne 0 ] \
  || [ "$report_mode" != 600 ] || [ "$report_links" -ne 1 ]; then
  echo "manual firmware-bootstrap report failed private-file invariants" >&2
  exit 73
fi
printf 'durable manual firmware-bootstrap report: %s\n' "$report"

set +e
initial_watchdog_status=$("$wifi_lab_watchdog" status 2>>"$report")
initial_watchdog_rc=$?
set -e
if [ "$initial_watchdog_rc" -ne 0 ] || [ "$initial_watchdog_status" != disarmed ]; then
  echo "installed np-only firmware-bootstrap watchdog was not initially disarmed" >>"$report"
  exit 75
fi
watchdog_before=$(@date@ +%s)
set +e
watchdog_token=$("$wifi_lab_watchdog" arm 2>>"$report")
watchdog_arm_rc=$?
watchdog_status=$("$wifi_lab_watchdog" status 2>>"$report")
watchdog_status_rc=$?
watchdog_after=$(@date@ +%s)
set -e
if [ "$watchdog_arm_rc" -ne 0 ] || [ "$watchdog_status_rc" -ne 0 ] \
  || ! printf '%s\n' "$watchdog_token" | @grep@ -Eq '^[0-9a-f]{32}$' \
  || [ "$(@wc@ -l <<EOF
$watchdog_status
EOF
)" -ne 4 ] \
  || [ "$(printf '%s\n' "$watchdog_status" | @grep@ -Ec '^armed deadline=[0-9]+$')" -ne 1 ] \
  || [ "$(printf '%s\n' "$watchdog_status" | @grep@ -Fxc 'ActiveState=active')" -ne 1 ] \
  || [ "$(printf '%s\n' "$watchdog_status" | @grep@ -Fxc 'SubState=waiting')" -ne 1 ] \
  || [ "$(printf '%s\n' "$watchdog_status" | @grep@ -Ec '^NextElapseUSecMonotonic=.+$')" -ne 1 ]; then
  echo "installed np-only firmware-bootstrap watchdog did not arm cleanly" >>"$report"
  exit 75
fi
watchdog_deadline=$(printf '%s\n' "$watchdog_status" | @grep@ -E '^armed deadline=[0-9]+$')
watchdog_deadline=${watchdog_deadline#armed deadline=}
if [ "$watchdog_deadline" -lt "$((watchdog_before + 120))" ] \
  || [ "$watchdog_deadline" -gt "$((watchdog_after + 120))" ] \
  || [ "$watchdog_deadline" -le "$watchdog_after" ]; then
  echo "installed np-only firmware-bootstrap watchdog did not provide its fixed 120-second lease" >>"$report"
  exit 75
fi
printf 'MANUAL_BOOTSTRAP watchdog=armed lease_seconds=120 host=np-only\n' >>"$report"

set +e
"$wifi_driver_lab" --run "$state" "$bdf" -- \
  "$driver" --run-one-shot-fwdl --watchdog-armed >>"$report" 2>&1
child_rc=$?
set -e

certification='^\{"production_firmware_bootstrap":"passed","recovery":"np-watchdog","watchdog":"armed","single_run":true,"reset_generation":([0-9]|[1-9][0-9]+)\}$'
all_marker_count=$(@grep@ -Fc '"production_firmware_bootstrap"' "$report" || true)
valid_marker_count=$(@grep@ -Ec "$certification" "$report" || true)
eligible=false
if [ -f "$state" ] && [ ! -L "$state" ] && [ -f "$state.safety" ] \
  && [ ! -L "$state.safety" ]; then
  read -r state_uid state_mode state_links <<EOF
$(@stat@ -Lc '%u %a %h' -- "$state")
EOF
  read -r safety_uid safety_mode safety_links <<EOF
$(@stat@ -Lc '%u %a %h' -- "$state.safety")
EOF
  state_line_count=$(@wc@ -l < "$state")
  state_byte_count=$(@wc@ -c < "$state")
  state_contents=$(@cat@ "$state")
  state_records_valid=false
  case "$state_contents" in
    "IWD"$'\t'"true"$'\t-\t-'$'\n'"PCI"$'\t'"$bdf"$'\t'"mt7921e"$'\t'"mt7921e")
      [ "$state_line_count:$state_byte_count" = 2:46 ] && state_records_valid=true
      ;;
    "IWD"$'\t'"false"$'\t-\t-'$'\n'"PCI"$'\t'"$bdf"$'\t'"mt7921e"$'\t'"mt7921e")
      [ "$state_line_count:$state_byte_count" = 2:47 ] && state_records_valid=true
      ;;
  esac
  if [ "$state_uid:$state_mode:$state_links" = 0:600:1 ] \
    && [ "$safety_uid:$safety_mode:$safety_links" = 0:600:1 ] \
    && [ "$(@wc@ -c < "$state.safety")" -eq 5 ] \
    && @grep@ -Fxq SAFE "$state.safety" && [ "$state_records_valid" = true ]; then
    eligible=true
  fi
fi

if [ "$child_rc" -ne 0 ] || [ "$all_marker_count" -ne 1 ] \
  || [ "$valid_marker_count" -ne 1 ] || [ "$eligible" != true ]; then
  printf 'MANUAL_BOOTSTRAP retain=true child_rc=%s all_markers=%s valid_markers=%s eligible=%s state=%s\n' \
    "$child_rc" "$all_marker_count" "$valid_marker_count" "$eligible" "$state" >>"$report"
  printf 'MANUAL_BOOTSTRAP watchdog=retained reason=worker_or_certification_failure\n' >>"$report"
  if [ "$child_rc" -ne 0 ]; then
    exit "$child_rc"
  fi
  exit 75
fi

printf 'MANUAL_BOOTSTRAP certified=true restore=begin state=%s\n' "$state" >>"$report"
set +e
"$wifi_driver_lab" --restore "$state" >>"$report" 2>&1
restore_rc=$?
set -e
if [ "$restore_rc" -ne 0 ]; then
  printf 'MANUAL_BOOTSTRAP restore=failed rc=%s state_retained=true\n' "$restore_rc" >>"$report"
  printf 'MANUAL_BOOTSTRAP watchdog=retained reason=restore_failure\n' >>"$report"
  exit "$restore_rc"
fi
if [ -e "$state" ] || [ -L "$state" ] || [ -e "$state.safety" ] || [ -L "$state.safety" ]; then
  printf 'MANUAL_BOOTSTRAP restore=invalid rc=0 state_retained=true\n' >>"$report"
  printf 'MANUAL_BOOTSTRAP watchdog=retained reason=restore_state_remained\n' >>"$report"
  exit 75
fi
if ! "$wifi_driver_lab" --native-ready "$bdf" >>"$report" 2>&1; then
  printf 'MANUAL_BOOTSTRAP native_ready=false watchdog=retained\n' >>"$report"
  printf 'MANUAL_BOOTSTRAP watchdog=retained reason=native_connectivity_failure\n' >>"$report"
  exit 75
fi
set +e
"$wifi_lab_watchdog" disarm "$watchdog_token" >>"$report" 2>&1
watchdog_disarm_rc=$?
final_watchdog_status=$("$wifi_lab_watchdog" status 2>>"$report")
final_watchdog_status_rc=$?
set -e
if [ "$watchdog_disarm_rc" -ne 0 ] || [ "$final_watchdog_status_rc" -ne 0 ] \
  || [ "$final_watchdog_status" != disarmed ]; then
  printf 'MANUAL_BOOTSTRAP watchdog=disarm_failed\n' >>"$report"
  exit 75
fi
printf 'MANUAL_BOOTSTRAP restore=passed state_removed=true native_ready=true watchdog=disarmed\n' >>"$report"
