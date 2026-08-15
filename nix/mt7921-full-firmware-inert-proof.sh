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
expected_hashes=@expected_hashes@
closure_roots=@closure_roots@
closure_manifest_sha256=@closure_manifest_sha256@
manifest_verifier=@manifest_verifier@
runtime=/run/current-system/sw/bin

if [ "${1-}" = --plan ] && [ "$#" -eq 1 ]; then
  cat <<EOF
INERT_PROOF_PLAN schema=$schema hardware_handoff=false active_validation=false
IDENTITY project_core_source_sha256=@project_core@ composite_artifact_source_sha256=@composite_source@ bss_wire_contract=connac2-bss-wire-v1 basic_tlv_len=32 initial_payload_len=36 initial_command_len=84 associated_payload_len=44 associated_command_len=92 qbss_payload_offset=36 dtim_source=selected-beacon-shared-basic-bcnft
PASSIVE_M1_TELEMETRY contract=linux-6.18.40-passive-m1-rx-v6 safe_read_registers=0xd4208,0xd4528,0xd452c,0x820d8108,0x820e5000,0x820e5004 consuming_mib_reads=false snapshot_boundaries=before-post-assoc-tail,m1-observation-timeout-5000ms positive_result=target_m1_observed_at_rx_dma negative_result=no_m1_at_rx_dma_ambiguous target_scope=pinned-ap-to-client-exact-addr1-addr2-addr3-direction-and-eapol-key-m1 behavior=best-effort-read-only-telemetry,observer-deadline-5000ms,validation-only-initial-rsna-response-timeout-6000ms,normal-mode-timeouts-unchanged attribution_limit=independent-ap-or-over-air-witness-required
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

umask 077
@install@ -d -m700 /var/lib/wifi-driver-lab
snapshot() {
  target=$1
  tmp="$target.tmp"
  raw=${target%.normalized}.raw
  raw_tmp="$raw.tmp"
  rm -f "$tmp" "$raw_tmp"
  if ! host=$(@hostname@) ||
     ! boot_id=$(cat /proc/sys/kernel/random/boot_id) ||
     ! driver_link=$(readlink /sys/bus/pci/devices/$bdf/driver) ||
     ! pci_driver=$(basename "$driver_link") ||
     ! pci_power=$(cat /sys/bus/pci/devices/$bdf/power_state) ||
     ! pci_runtime=$(cat /sys/bus/pci/devices/$bdf/power/runtime_status); then
    rm -f "$tmp" "$raw_tmp"
    return 1
  fi
  capture_status=0
  {
    echo "SCHEMA=$schema"
    echo "HOST=$host"
    echo "BOOT_ID=$boot_id"
    echo "PCI_DRIVER=$pci_driver"
    echo "PCI_POWER=$pci_power"
    echo "PCI_RUNTIME=$pci_runtime"
    "$runtime/systemctl" show -p ActiveState -p SubState -p UnitFileState NetworkManager || capture_status=1
    "$runtime/ip" -o link show || capture_status=1
    "$runtime/ip" -o -4 address show || capture_status=1
    "$runtime/ip" -4 route show table all || capture_status=1
    "$runtime/iw" dev || capture_status=1
    if interfaces=$(ls /sys/class/net); then
      for net in $interfaces; do "$runtime/iw" dev "$net" link 2>/dev/null || true; done
    else
      capture_status=1
    fi
    printf WATCHDOG_STATUS=
    "$runtime/wifi-lab-watchdog" status || capture_status=1
  } >"$raw_tmp"
  if [ "$capture_status" -ne 0 ] ||
     ! @sed@ -E '/^[[:space:]]*(RX:|TX:|signal:|rx bitrate:|tx bitrate:)/d; s/qlen [0-9]+//g; s/valid_lft [^ ]+/valid_lft DYNAMIC/g; s/preferred_lft [^ ]+/preferred_lft DYNAMIC/g' "$raw_tmp" >"$tmp" ||
     ! mv "$raw_tmp" "$raw" ||
     ! mv "$tmp" "$target"; then
    rm -f "$tmp" "$raw_tmp"
    return 1
  fi
}

phase=initialization
token=
finish() {
  status=$?
  trap - EXIT HUP INT TERM
  set +e
  if [ -n "$token" ]; then
    "$runtime/wifi-lab-watchdog" disarm "$token" >"$out/watchdog-disarm.txt" 2>&1
    if [ "$?" -ne 0 ]; then status=1; fi
    token=
  fi
  watchdog_status=$($runtime/wifi-lab-watchdog status 2>&1)
  printf '%s\n' "$watchdog_status" >"$out/watchdog-final-status.txt"
  timer_status=$("$runtime/systemctl" is-active wifi-lab-watchdog.timer 2>&1)
  printf '%s\n' "$timer_status" >"$out/watchdog-timer-status.txt"
  if [ "$watchdog_status" != disarmed ]; then status=1; fi
  if [ "$timer_status" != inactive ]; then status=1; fi
  if [ -f "$out/state.before.normalized" ] && [ ! -f "$out/state.after.normalized" ]; then
    if ! snapshot "$out/state.after.normalized"; then
      rm -f "$out/state.after.normalized"
      echo unavailable >"$out/state-after-status.txt"
      status=1
    fi
  fi
  if [ -f "$out/state.before.normalized" ] && [ -f "$out/state.after.normalized" ]; then
    if cmp "$out/state.before.normalized" "$out/state.after.normalized"; then
      echo passed >"$out/state-equality.txt"
      : >"$out/state-diff.txt"
    else
      @diff@ -u "$out/state.before.normalized" "$out/state.after.normalized" >"$out/state-diff.txt"
      echo failed >"$out/state-equality.txt"
      status=1
    fi
  else
    echo unavailable >"$out/state-equality.txt"
  fi
  result=failed
  if [ "$status" -eq 0 ]; then result=passed; fi
  write_summary() {
    printf 'SCHEMA=%s\nRESULT=%s\nEXIT_STATUS=%s\nPHASE=%s\n' \
      "$schema" "$result" "$status" "$phase" >"$out/SUMMARY"
  }
  if ! write_summary; then
    status=1
    result=failed
    write_summary || true
  fi
  if find "$out" -maxdepth 1 -type f ! -name 'SHA256SUMS*' -print0 | sort -z | \
    xargs -0 @sha256sum@ >"$out/SHA256SUMS.tmp" &&
    mv "$out/SHA256SUMS.tmp" "$out/SHA256SUMS"; then
    :
  else
    status=1
    result=failed
    write_summary
    rm -f "$out/SHA256SUMS.tmp"
    if find "$out" -maxdepth 1 -type f ! -name 'SHA256SUMS*' -print0 | sort -z | \
      xargs -0 @sha256sum@ >"$out/SHA256SUMS.tmp" &&
      mv "$out/SHA256SUMS.tmp" "$out/SHA256SUMS"; then
      :
    else
      rm -f "$out/SHA256SUMS.tmp"
      echo failed >"$out/SHA256SUMS.failure"
    fi
  fi
  if [ "$status" -eq 0 ]; then echo "$out"; fi
  exit "$status"
}
out=$(@mktemp@ -d /var/lib/wifi-driver-lab/@source_identity@-inert-proof-$(@date@ -u +%Y%m%dT%H%M%SZ)-XXXXXX)
trap finish EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
cp "$expected_hashes" "$out/closure.expected.tsv"
cp "$closure_roots" "$out/closure.roots"
printf 'SCHEMA=%s\nSOURCE_IDENTITY_SHA256=%s\nPROJECT_CORE_SOURCE_SHA256=%s\nCOMPOSITE_ARTIFACT_SOURCE_SHA256=%s\nBSS_WIRE_CONTRACT=connac2-bss-wire-v1\nPASSIVE_M1_TELEMETRY_CONTRACT=linux-6.18.40-passive-m1-rx-v6\nASSOCIATION_REQUEST_CONTRACT=mt7921-supported-subset-v2\nCANONICAL_ASSOCIATION_FIXTURE_SHA256=5449fa5acf5317259694bb400a04d6ba8e169f99cf555583a424b3530f8a63c4\nRUNTIME_ASSOCIATION_HASH_POLICY=input-dependent\nASSOCIATION_CAPABILITY_INPUT_SOURCE=firmware-nic-capability+pinned-regdb-to-softmac-query-band-v2\nASSOCIATION_TRANSFORMATION_CONTRACT=device+pinned-regdb-authoritative-association-v2\nORACLE_COMPARISON_CONTRACT=linux-6.18.40-semantic-v1\nORACLE_COMPARISON_NORMALIZED_SHA256=6a80b1b8631d70447f20b1be45a35564a806bc8913848d9fdb51c3404ddf4755\nEARLY_M1_LATCH_CONTRACT=exact-m1-one-frame-epoch-v1\nEARLY_M1_DUPLICATE_POLICY=same-replay-and-byte-identical-complete-frame-ignore;changed-byte-or-replay-poisons-containment\nSAFE_READ_REGISTERS=0xd4208,0xd4528,0xd452c,0x820d8108,0x820e5000,0x820e5004\nCONSUMING_MIB_READS=false\nSNAPSHOT_BOUNDARIES=before-post-assoc-tail,m1-observation-timeout-5000ms\nPOSITIVE_RESULT=target_m1_observed_at_rx_dma\nNEGATIVE_RESULT=no_m1_at_rx_dma_ambiguous\nTARGET_SCOPE=pinned-ap-to-client-exact-addr1-addr2-addr3-direction-and-eapol-key-m1\nTELEMETRY_BEHAVIOR=best-effort-read-only-telemetry,observer-deadline-5000ms,validation-only-initial-rsna-response-timeout-6000ms,normal-mode-timeouts-unchanged\nATTRIBUTION_LIMIT=independent-ap-or-over-air-witness-required\nTARGET_BEACON_TIM_CONTRACT=linux-ieee80211-check-tim-v1\nTIM_TRUE_RESULT=ap-queued-unicast-for-normalized-aid-not-traffic-type\nTIM_NEVER_TRUE_RESULT=inconclusive\nTOOL=%s\nCLOSURE_MANIFEST_SHA256=%s\n' \
  "$schema" @source_identity@ @project_core@ @composite_source@ "$0" "$closure_manifest_sha256" >"$out/IDENTITY"

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

phase=before-snapshot
snapshot "$out/state.before.normalized"
phase=closure-manifest
test "$(@sha256sum@ "$out/closure.expected.tsv" | @cut@ -d' ' -f1)" = "$closure_manifest_sha256"
"$manifest_verifier" "$out/closure.expected.tsv" "$out/closure.roots" "$out" @nix_store@
phase=self-tests
@sha256sum@ "$launcher" "$driver" "$supervisor/bin/mt7921-full-firmware-validation-supervisor" "$manifest" "$root_entry/bin/mt7921-full-firmware-validation-root" "$0" >"$out/artifact-hashes.txt"
env -i "$driver" --self-test-rate-power-delivery >"$out/selftest-rate.jsonl"
env -i "$driver" --self-test-production-validation >"$out/selftest-production.jsonl"
grep -F '"rate_power_self_test":"passed"' "$out/selftest-rate.jsonl" >/dev/null
grep -F '"production_validation_self_test":"passed"' "$out/selftest-production.jsonl" >/dev/null
grep -F '"bss_wire_contract":"connac2-bss-wire-v1"' "$out/selftest-production.jsonl" >/dev/null
grep -F '"associated_bss_command_len":92' "$out/selftest-production.jsonl" >/dev/null
grep -F '"associated_bss_command_sha256":"6ea81837d7eb1aabe44edace8f8d8d280a60d48249fc2352e9a24a10390a9cc5"' "$out/selftest-production.jsonl" >/dev/null
"$root_entry/bin/mt7921-full-firmware-validation-root" --plan >"$out/root-plan.txt"
grep -F hardware_handoff=false "$out/root-plan.txt" >/dev/null

phase=watchdog-arm
token=$("$runtime/wifi-lab-watchdog" arm)
echo armed_for=inert_full_firmware_preflight >"$out/watchdog-arm.txt"
phase=canonical-preflight
env -i "$launcher" --full-firmware-preflight >"$out/full-firmware-preflight.jsonl" 2>"$out/full-firmware-preflight.stderr"
phase=watchdog-disarm
"$runtime/wifi-lab-watchdog" disarm "$token" >"$out/watchdog-disarm.txt"
token=
phase=preflight-assertions
for marker in '"full_firmware_preflight":"passed"' '"artifact_flavor":"full-firmware-production"' '"enabled_operation":"run-one-shot-sae-auth"' '"active_capable":true' '"fd_contract":"credential-fd3+snapshot-fd4+immediate-eof"' '"credential_eof":true' '"credential_policy_binding":"validated-and-consumed-before-device-open"' '"snapshot_eof":true' '"regulatory_domain":"00"' '"regulatory_generation":0' '"device_opened":false' '"vfio_opened":false' '"lab_state_created":false'; do
  grep -F "$marker" "$out/full-firmware-preflight.jsonl" >/dev/null
done
test "$($runtime/wifi-lab-watchdog status)" = disarmed
phase=after-snapshot
snapshot "$out/state.after.normalized"
phase=state-compare
cmp "$out/state.before.normalized" "$out/state.after.normalized"
echo passed >"$out/state-equality.txt"
: >"$out/state-diff.txt"
phase=complete
