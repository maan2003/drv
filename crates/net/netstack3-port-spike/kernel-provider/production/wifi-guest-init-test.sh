#!/usr/bin/env bash
set -euo pipefail
guest_init=${1:-$(dirname "$0")/wifi-guest-init}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

awk '
  /^unset dns_pid$/ { copy = 1 }
  copy { print }
  /^echo PASS_MT_GUEST_INTERNET$/ { exit }
' "$guest_init" |
    sed \
        -e "s|/run/driver.safety|$tmp/driver.safety|g" \
        -e "s|/run/stack.log|$tmp/stack.log|g" >"$tmp/finish"
grep -Fx 'kill -TERM "$driver" 2>/dev/null || true' "$tmp/finish"
grep -Fx 'wait "$driver"' "$tmp/finish"

cat >"$tmp/launcher" <<'SH'
#!/bin/sh
set -eu
safety=$1
observed=$2
ready=$3
exit_rc=$4
finish() {
    echo TERM >"$observed"
    echo SAFE >"$safety"
    exit "$exit_rc"
}
trap finish TERM
echo READY >"$ready"
while :; do sleep 1; done
SH
chmod +x "$tmp/launcher"
: >"$tmp/stack.log"

run() {
    local exit_rc=$1 output rc
    rm -f "$tmp/observed" "$tmp/ready"
    echo MUTATED >"$tmp/driver.safety"
    set +e
    output=$(
        "$tmp/launcher" "$tmp/driver.safety" "$tmp/observed" "$tmp/ready" "$exit_rc" &
        driver=$!
        while test ! -e "$tmp/ready"; do sleep .01; done
        result=0
        . "$tmp/finish"
    )
    rc=$?
    set -e
    test -e "$tmp/observed"
    printf '%s\n' "$output"
    return "$rc"
}

output=$(run 0)
grep -q '^GUEST_DRIVER_EXIT=0 safety=SAFE$' <<<"$output"
grep -q '^GUEST_MT_HARDWARE_SAFE$' <<<"$output"
grep -q '^PASS_MT_GUEST_INTERNET$' <<<"$output"
echo PASS_WIFI_GUEST_ORDERLY_STOP

set +e
output=$(run 7)
rc=$?
set -e
test "$rc" -ne 0
grep -q '^GUEST_DRIVER_EXIT=7 safety=SAFE$' <<<"$output"
if grep -q '^PASS_MT_GUEST_INTERNET$' <<<"$output"; then
    echo "failed driver shutdown emitted overall PASS" >&2
    exit 1
fi
echo PASS_WIFI_GUEST_FAILED_SHUTDOWN_NO_OVERALL_PASS
