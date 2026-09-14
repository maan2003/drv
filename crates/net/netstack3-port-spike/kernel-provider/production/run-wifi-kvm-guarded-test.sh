#!/usr/bin/env bash
set -euo pipefail
wrapper=${1:-$(dirname "$0")/run-wifi-kvm-guarded.sh}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cat >"$tmp/sudo" <<'SH'
#!/bin/sh
set -eu
if test "${2-}" = --quarantined; then
    case "$MOCK_SCENARIO" in
        preflight-sudo-rc1) exit 1 ;;
        final-sudo-rc1)
            count=$(cat "$MOCK_SUDO_QUARANTINE_COUNT" 2>/dev/null || echo 0)
            count=$((count + 1))
            echo "$count" >"$MOCK_SUDO_QUARANTINE_COUNT"
            test "$count" -lt 2 || exit 1
            ;;
    esac
fi
exec "$@"
SH
cat >"$tmp/watchdog" <<'SH'
#!/bin/sh
set -eu
case "$1" in
    status)
        if test -e "$MOCK_WATCHDOG_STATE"; then echo armed; else echo disarmed; fi
        ;;
    arm)
        : >"$MOCK_WATCHDOG_STATE"
        echo arm >>"$MOCK_LOG"
        echo token
        ;;
    heartbeat)
        test -e "$MOCK_WATCHDOG_STATE"
        echo heartbeat >>"$MOCK_LOG"
        if test "$MOCK_SCENARIO" = heartbeat-fail; then exit 1; fi
        if test "$MOCK_SCENARIO" = late-heartbeat-fail; then
            count=$(cat "$MOCK_HEARTBEAT_COUNT" 2>/dev/null || echo 0)
            count=$((count + 1))
            echo "$count" >"$MOCK_HEARTBEAT_COUNT"
            test "$count" -lt 2
        fi
        ;;
    disarm)
        rm -f "$MOCK_WATCHDOG_STATE"
        echo disarm >>"$MOCK_LOG"
        ;;
    *) exit 2 ;;
esac
SH
cat >"$tmp/lab" <<'SH'
#!/bin/sh
set -eu
case "$1" in
    --idle) test ! -e "$MOCK_STATE" ;;
    --quarantined)
        case "$MOCK_SCENARIO" in
            preflight-unsafe) exit 0 ;;
            preflight-query-error) exit 2 ;;
            quarantine-query-error)
                count=$(cat "$MOCK_QUARANTINE_COUNT" 2>/dev/null || echo 0)
                count=$((count + 1))
                echo "$count" >"$MOCK_QUARANTINE_COUNT"
                test "$count" -lt 2 || exit 2
                ;;
        esac
        if test -e "$MOCK_STATE"; then
            exit 0
        fi
        echo clear
        exit 1
        ;;
    --native-ready)
        if test "$MOCK_SCENARIO" = late-heartbeat-fail && test -e "$MOCK_RESTORING"; then
            exit 1
        fi
        exit 0
        ;;
    *)
        echo launch >>"$MOCK_LOG"
        case "$MOCK_SCENARIO" in
            success|preflight-sudo-rc1|final-sudo-rc1) exit 0 ;;
            safe-fail) exit 7 ;;
            quarantine) : >"$MOCK_STATE"; exit 9 ;;
            signal-safe|heartbeat-fail) sleep 2; exit 0 ;;
            late-heartbeat-fail) : >"$MOCK_RESTORING"; exit 0 ;;
            quarantine-query-error) exit 0 ;;
        esac
        ;;
esac
SH
cat >"$tmp/work" <<'SH'
#!/bin/sh
exit 0
SH
chmod +x "$tmp/sudo" "$tmp/watchdog" "$tmp/lab" "$tmp/work"

run() {
    local scenario=$1 expected=$2
    rm -f "$tmp/state" "$tmp/log" "$tmp/watchdog-state" "$tmp/heartbeat-count" "$tmp/quarantine-count" "$tmp/sudo-quarantine-count" "$tmp/restoring"
    local rc=0
    DRV_SAE_BSSID=02:00:00:00:00:01 DRV_SAE_CHANNEL=149 \
    SUDO="$tmp/sudo" WIFI_DRIVER_LAB="$tmp/lab" WIFI_LAB_WATCHDOG="$tmp/watchdog" \
    WIFI_HEARTBEAT_SECONDS=1 MOCK_LOG="$tmp/log" MOCK_STATE="$tmp/state" MOCK_WATCHDOG_STATE="$tmp/watchdog-state" MOCK_HEARTBEAT_COUNT="$tmp/heartbeat-count" MOCK_QUARANTINE_COUNT="$tmp/quarantine-count" MOCK_SUDO_QUARANTINE_COUNT="$tmp/sudo-quarantine-count" MOCK_RESTORING="$tmp/restoring" \
    MOCK_SCENARIO="$scenario" "$wrapper" 0000:05:00.0 140 -- "$tmp/work" || rc=$?
    test "$rc" = "$expected"
}
run preflight-unsafe 75
if test -e "$tmp/log" && grep -Eq '^(arm|launch)$' "$tmp/log"; then
    echo "unsafe preflight armed watchdog or launched lab" >&2
    exit 1
fi
test ! -e "$tmp/watchdog-state"
echo PASS_GUARDED_WRAPPER_PREFLIGHT_UNSAFE_REFUSES_LAUNCH

run preflight-query-error 75
if test -e "$tmp/log" && grep -Eq '^(arm|launch)$' "$tmp/log"; then
    echo "failed preflight query armed watchdog or launched lab" >&2
    exit 1
fi
test ! -e "$tmp/watchdog-state"
echo PASS_GUARDED_WRAPPER_PREFLIGHT_QUERY_ERROR_REFUSES_LAUNCH

run preflight-sudo-rc1 75
if test -e "$tmp/log" && grep -Eq '^(arm|launch)$' "$tmp/log"; then
    echo "sudo rc1 preflight armed watchdog or launched lab" >&2
    exit 1
fi
test ! -e "$tmp/watchdog-state"
echo PASS_GUARDED_WRAPPER_PREFLIGHT_SUDO_RC1_REFUSES_LAUNCH

run success 0
test "$(grep -c '^disarm$' "$tmp/log")" = 1
test ! -e "$tmp/watchdog-state"
echo PASS_GUARDED_WRAPPER_SUCCESS

run safe-fail 7
test "$(grep -c '^disarm$' "$tmp/log")" = 1
test ! -e "$tmp/watchdog-state"
echo PASS_GUARDED_WRAPPER_SAFE_FAILURE

run quarantine 9
if grep -q '^disarm$' "$tmp/log"; then
    echo "unexpected watchdog disarm" >&2
    exit 1
fi
test -e "$tmp/state"
test -e "$tmp/watchdog-state"
echo PASS_GUARDED_WRAPPER_QUARANTINE_RETAINS_DEADLINE

run heartbeat-fail 75
if grep -q '^disarm$' "$tmp/log"; then
    echo "unexpected watchdog disarm" >&2
    exit 1
fi
grep -q '^heartbeat$' "$tmp/log"
test -e "$tmp/watchdog-state"
echo PASS_GUARDED_WRAPPER_HEARTBEAT_FAILURE_RETAINS_DEADLINE

run late-heartbeat-fail 75
if grep -q '^disarm$' "$tmp/log"; then
    echo "unexpected watchdog disarm" >&2
    exit 1
fi
test "$(cat "$tmp/heartbeat-count")" = 2
test -e "$tmp/watchdog-state"
echo PASS_GUARDED_WRAPPER_LATE_HEARTBEAT_FAILURE_RETAINS_DEADLINE

run quarantine-query-error 75
if grep -q '^disarm$' "$tmp/log"; then
    echo "unexpected watchdog disarm" >&2
    exit 1
fi
test -e "$tmp/watchdog-state"
echo PASS_GUARDED_WRAPPER_QUARANTINE_QUERY_ERROR_RETAINS_DEADLINE

run final-sudo-rc1 75
if grep -q '^disarm$' "$tmp/log"; then
    echo "sudo rc1 caused watchdog disarm" >&2
    exit 1
fi
grep -q '^launch$' "$tmp/log"
test -e "$tmp/watchdog-state"
echo PASS_GUARDED_WRAPPER_FINAL_SUDO_RC1_RETAINS_DEADLINE

rm -f "$tmp/state" "$tmp/log" "$tmp/watchdog-state" "$tmp/heartbeat-count" "$tmp/quarantine-count" "$tmp/sudo-quarantine-count" "$tmp/restoring"
set +e
DRV_SAE_BSSID=02:00:00:00:00:01 DRV_SAE_CHANNEL=149 \
SUDO="$tmp/sudo" WIFI_DRIVER_LAB="$tmp/lab" WIFI_LAB_WATCHDOG="$tmp/watchdog" \
WIFI_HEARTBEAT_SECONDS=1 MOCK_LOG="$tmp/log" MOCK_STATE="$tmp/state" MOCK_WATCHDOG_STATE="$tmp/watchdog-state" MOCK_HEARTBEAT_COUNT="$tmp/heartbeat-count" MOCK_QUARANTINE_COUNT="$tmp/quarantine-count" MOCK_SUDO_QUARANTINE_COUNT="$tmp/sudo-quarantine-count" MOCK_RESTORING="$tmp/restoring" \
MOCK_SCENARIO=signal-safe "$wrapper" 0000:05:00.0 140 -- "$tmp/work" &
pid=$!
sleep .2
kill -TERM "$pid"
wait "$pid"
rc=$?
set -e
test "$rc" = 143
test "$(grep -c '^disarm$' "$tmp/log")" = 1
test ! -e "$tmp/watchdog-state"
echo PASS_GUARDED_WRAPPER_SIGNAL_WAITS_FOR_SAFE_RESTORE
