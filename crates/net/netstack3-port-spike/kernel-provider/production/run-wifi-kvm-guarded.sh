#!/usr/bin/env bash
# Arm and retain host reboot recovery around a wifi-driver-lab command.
set -euo pipefail
bdf=${1:?usage: run-wifi-kvm-guarded.sh PCI_BDF LAB_TIMEOUT -- COMMAND [ARG ...]}
lab_timeout=${2:?}
test "${3-}" = -- || exit 2
shift 3
test "$#" -gt 0 || exit 2
: "${DRV_SAE_BSSID:?}"
: "${DRV_SAE_CHANNEL:?}"
wifi_driver_lab=${WIFI_DRIVER_LAB:-/run/current-system/sw/bin/wifi-driver-lab}
wifi_lab_watchdog=${WIFI_LAB_WATCHDOG:-/run/current-system/sw/bin/wifi-lab-watchdog}
heartbeat_seconds=${WIFI_HEARTBEAT_SECONDS:-20}
sudo_command=${SUDO:-sudo}
case "$heartbeat_seconds" in
    ''|*[!0-9]*|0) exit 2 ;;
esac
test "$("$sudo_command" "$wifi_lab_watchdog" status)" = disarmed
"$sudo_command" "$wifi_driver_lab" --idle
set +e
quarantine_status=$("$sudo_command" "$wifi_driver_lab" --quarantined)
quarantine_rc=$?
set -e
test "$quarantine_rc" -eq 1 && test "$quarantine_status" = clear || exit 75
"$sudo_command" "$wifi_driver_lab" --native-ready "$bdf"

token=$("$sudo_command" "$wifi_lab_watchdog" arm)
heartbeat_pid=
heartbeat_failure=$(mktemp)
heartbeat_stop=$(mktemp)
rm -f "$heartbeat_failure" "$heartbeat_stop"
experiment_pid=
requested_exit=
cleanup() {
    set +e
    if test -n "$heartbeat_pid"; then
        kill "$heartbeat_pid" 2>/dev/null || true
        wait "$heartbeat_pid" 2>/dev/null || true
    fi
    rm -f "$heartbeat_failure" "$heartbeat_stop"
}
record_signal() {
    requested_exit=$1
}
trap cleanup EXIT
trap 'record_signal 130' INT
trap 'record_signal 143' TERM
(
    while sleep "$heartbeat_seconds"; do
        test ! -e "$heartbeat_stop" || exit 0
        if ! "$sudo_command" "$wifi_lab_watchdog" heartbeat "$token"; then
            : >"$heartbeat_failure"
            exit 1
        fi
    done
) &
heartbeat_pid=$!

"$sudo_command" env DRV_SAE_BSSID="$DRV_SAE_BSSID" DRV_SAE_CHANNEL="$DRV_SAE_CHANNEL" \
    "$wifi_driver_lab" "$bdf" "$lab_timeout" -- "$@" &
experiment_pid=$!
experiment_rc=
while test -z "$experiment_rc"; do
    set +e
    wait "$experiment_pid"
    wait_rc=$?
    set -e
    if ! kill -0 "$experiment_pid" 2>/dev/null; then
        experiment_rc=$wait_rc
    fi
done
experiment_pid=

# A failed heartbeat invalidates the guarded lifecycle even if restoration later
# succeeds. Leave the deadline armed rather than claiming a protected run.
if test -e "$heartbeat_failure" || ! kill -0 "$heartbeat_pid" 2>/dev/null; then
    wait "$heartbeat_pid" 2>/dev/null || true
    heartbeat_pid=
    exit 75
fi

restored=false
for _ in $(seq 1 90); do
    if test -e "$heartbeat_failure" || ! kill -0 "$heartbeat_pid" 2>/dev/null; then
        exit 75
    fi
    set +e
    quarantine_status=$("$sudo_command" "$wifi_driver_lab" --quarantined)
    quarantine_rc=$?
    set -e
    case "$quarantine_rc" in
        0)
            test -n "$requested_exit" && exit "$requested_exit"
            test "$experiment_rc" -ne 0 && exit "$experiment_rc"
            exit 75
            ;;
        1) test "$quarantine_status" = clear || exit 75 ;;
        *) exit 75 ;;
    esac
    if "$sudo_command" "$wifi_driver_lab" --idle &&
       "$sudo_command" "$wifi_driver_lab" --native-ready "$bdf"; then
        restored=true
        break
    fi
    sleep 1
done
$restored || exit 75
: >"$heartbeat_stop"
set +e
wait "$heartbeat_pid"
heartbeat_rc=$?
set -e
heartbeat_pid=
if test "$heartbeat_rc" -ne 0 || test -e "$heartbeat_failure"; then
    exit 75
fi
"$sudo_command" "$wifi_lab_watchdog" disarm "$token"
token=
test -n "$requested_exit" && exit "$requested_exit"
exit "$experiment_rc"
