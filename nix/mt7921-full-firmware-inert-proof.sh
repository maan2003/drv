#!@shell@
set -euo pipefail

schema=mt7921-inert-proof-v1
bdf=0000:05:00.0
package=@package@
supervisor=@supervisor@
manifest=@manifest@
root_entry=@root_entry@
launcher="$package/bin/mt7921-full-firmware-validation"
driver="$package/libexec/mt7921-full-firmware-validation"
expected_paths=@expected_paths@
expected_hashes=@expected_hashes@
runtime=/run/current-system/sw/bin

if [ "${1-}" = --plan ] && [ "$#" -eq 1 ]; then
  cat <<EOF
INERT_PROOF_PLAN schema=$schema hardware_handoff=false active_validation=false
CAPTURE hostname; boot_id; pci_driver; pci_power_state; pci_runtime_status; NetworkManager_state; ip_link; ipv4_addresses; ipv4_routes; iw_dev; iw_link; watchdog_status
NORMALIZE drop_rx_tx_signal_bitrate; normalize_queue_length; normalize_address_lifetimes
WATCHDOG arm=wifi-lab-watchdog_arm disarm=wifi-lab-watchdog_disarm_exact_token trap_safe=true
PREFLIGHT argv=$launcher --full-firmware-preflight canonical_fd3_fd4=true
ASSERT credential_eof=true snapshot_eof=true credential_policy_binding=validated-and-consumed-before-device-open regulatory_domain=00 regulatory_generation=0 device_opened=false vfio_opened=false lab_state_created=false
COMPARE exact_cmp=true durable_sha256=true
EOF
  exit 0
fi
if [ "$#" -ne 0 ]; then
  echo "inert proof accepts no arguments except --plan" >&2
  exit 64
fi
if [ "$(@id@ -u)" -ne 0 ]; then
  echo "inert proof requires root" >&2
  exit 77
fi

@install@ -d -m700 /var/lib/wifi-driver-lab
out=$(@mktemp@ -d /var/lib/wifi-driver-lab/@commit@-inert-proof-$(@date@ -u +%Y%m%dT%H%M%SZ)-XXXXXX)
cp "$expected_paths" "$out/closure.expected.paths"
cp "$expected_hashes" "$out/closure.expected.tsv"
printf 'SCHEMA=%s\nCOMMIT=%s\nTOOL=%s\n' "$schema" @commit@ "$0" >"$out/IDENTITY"

snapshot() {
  {
    echo "SCHEMA=$schema"
    echo "HOST=$(@hostname@)"
    echo "BOOT_ID=$(cat /proc/sys/kernel/random/boot_id)"
    echo "PCI_DRIVER=$(basename "$(readlink /sys/bus/pci/devices/$bdf/driver)")"
    echo "PCI_POWER=$(cat /sys/bus/pci/devices/$bdf/power_state)"
    echo "PCI_RUNTIME=$(cat /sys/bus/pci/devices/$bdf/power/runtime_status)"
    "$runtime/systemctl" show -p ActiveState -p SubState -p UnitFileState NetworkManager
    "$runtime/ip" -o link show
    "$runtime/ip" -o -4 address show
    "$runtime/ip" -4 route show table all
    "$runtime/iw" dev
    for net in $(ls /sys/class/net); do "$runtime/iw" dev "$net" link 2>/dev/null || true; done
    printf WATCHDOG_STATUS=; "$runtime/wifi-lab-watchdog" status
  } | @sed@ -E '/^[[:space:]]*(RX:|TX:|signal:|rx bitrate:|tx bitrate:)/d; s/qlen [0-9]+//g; s/valid_lft [^ ]+/valid_lft DYNAMIC/g; s/preferred_lft [^ ]+/preferred_lft DYNAMIC/g' >"$1"
}

cat >"$out/COMMANDS" <<EOF
SCHEMA=$schema
SNAPSHOT=hostname;cat_/proc/sys/kernel/random/boot_id;readlink_pci_driver;cat_pci_power_state;cat_pci_runtime_status;systemctl_show_NetworkManager;ip_-o_link;ip_-o_-4_address;ip_-4_route_table_all;iw_dev;iw_dev_INTERFACE_link;wifi-lab-watchdog_status
NORMALIZE=sed_drop_RX_TX_signal_bitrate;queue_length_to_empty;address_lifetimes_to_DYNAMIC
PREFLIGHT=env_-i_launcher_--full-firmware-preflight
CLOSURE=nix-store_--verify-path_then_registered_hash_compare
SELFTEST=driver_--self-test-rate-power-delivery;driver_--self-test-production-validation
ROOT_PLAN=root-entry_--plan
WATCHDOG=arm;exact-token-disarm;status_must_equal_disarmed
COMPARE=cmp_before_after;diff_on_failure
DURABILITY=sha256sum_all_output_files_except_SHA256SUMS
EOF

snapshot "$out/state.before.normalized"
@nix_store@ -qR "$package" "$supervisor" "$manifest" "$root_entry" | sort -u >"$out/closure.actual.paths"
cmp "$out/closure.expected.paths" "$out/closure.actual.paths"
while IFS=$'\t' read -r path hash; do
  @nix_store@ --verify-path "$path"
  actual=$(@nix_store@ -q --hash "$path")
  test "$actual" = "$hash"
  printf '%s\t%s\n' "$path" "$actual"
done <"$out/closure.expected.tsv" >"$out/closure.verified.tsv"
@sha256sum@ "$launcher" "$driver" "$supervisor/bin/mt7921-full-firmware-validation-supervisor" "$manifest" "$root_entry/bin/mt7921-full-firmware-validation-root" "$0" >"$out/artifact-hashes.txt"
env -i "$driver" --self-test-rate-power-delivery >"$out/selftest-rate.jsonl"
env -i "$driver" --self-test-production-validation >"$out/selftest-production.jsonl"
grep -F '"rate_power_self_test":"passed"' "$out/selftest-rate.jsonl" >/dev/null
grep -F '"production_validation_self_test":"passed"' "$out/selftest-production.jsonl" >/dev/null
"$root_entry/bin/mt7921-full-firmware-validation-root" --plan >"$out/root-plan.txt"
grep -F hardware_handoff=false "$out/root-plan.txt" >/dev/null

token=
cleanup() {
  status=$?
  trap - EXIT
  if [ -n "$token" ]; then
    "$runtime/wifi-lab-watchdog" disarm "$token" >"$out/watchdog-disarm.txt" 2>&1 || status=1
  fi
  if [ "$($runtime/wifi-lab-watchdog status 2>&1)" != disarmed ]; then
    echo 'watchdog remained armed after inert proof cleanup' >>"$out/watchdog-disarm.txt"
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
token=$("$runtime/wifi-lab-watchdog" arm)
echo armed_for=inert_full_firmware_preflight >"$out/watchdog-arm.txt"
env -i "$launcher" --full-firmware-preflight >"$out/full-firmware-preflight.jsonl" 2>"$out/full-firmware-preflight.stderr"
"$runtime/wifi-lab-watchdog" disarm "$token" >"$out/watchdog-disarm.txt"
token=
trap - EXIT
trap - HUP INT TERM
for marker in '"full_firmware_preflight":"passed"' '"credential_eof":true' '"credential_policy_binding":"validated-and-consumed-before-device-open"' '"snapshot_eof":true' '"regulatory_domain":"00"' '"regulatory_generation":0' '"device_opened":false' '"vfio_opened":false' '"lab_state_created":false'; do
  grep -F "$marker" "$out/full-firmware-preflight.jsonl" >/dev/null
done
test "$($runtime/wifi-lab-watchdog status)" = disarmed
snapshot "$out/state.after.normalized"
if ! cmp "$out/state.before.normalized" "$out/state.after.normalized" >"$out/state-diff.txt" 2>&1; then
  diff -u "$out/state.before.normalized" "$out/state.after.normalized" >"$out/state-diff.txt" || true
  exit 1
fi
echo passed >"$out/state-equality.txt"
find "$out" -maxdepth 1 -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 @sha256sum@ >"$out/SHA256SUMS"
chmod -R go-rwx "$out"
echo "$out"
