#!@shell@
set -eu

case "$#:${1-}" in
  0:) operation= ;;
  1:--plan) operation=--plan ;;
  *)
    echo "fixed production root entrypoint accepts no arguments except --plan" >&2
    exit 64
    ;;
esac

@sha256sum@ @manifest@ >/dev/null
@grep@ -Fx 'FLAVOR=full-firmware-production' @manifest@ >/dev/null
@grep@ -Fx 'ACTIVE_CAPABLE=true' @manifest@ >/dev/null
@grep@ -Fx 'OBSERVATION_MODE=passive-m1-observation' @manifest@ >/dev/null
@grep@ -Fx 'FRAME_TX_DISABLED_BEFORE_M1=true' @manifest@ >/dev/null
@grep@ -Fx 'REQUIRED_PRE_M1_MANAGEMENT_TX=sae-and-association' @manifest@ >/dev/null
@grep@ -Fx 'PREASSOCIATION_PHYSICAL_TX_CLASSES=sae-authentication,association-request' @manifest@ >/dev/null
@grep@ -Fx 'POSTASSOCIATION_PHYSICAL_TX=disabled' @manifest@ >/dev/null
@grep@ -Fx 'MANAGEMENT_TX_TERMINAL_CONTRACT=acked-txs+successful-tx-free;drop-retires;timeout-poisons' @manifest@ >/dev/null
@grep@ -Fx 'POST_ASSOC_PUBLIC_TX=disabled-until-m1-observed' @manifest@ >/dev/null
@grep@ -Fx 'M2_PHYSICAL_TX=suppressed' @manifest@ >/dev/null
@grep@ -Fx 'BSS_WIRE_CONTRACT=connac2-bss-wire-v1' @manifest@ >/dev/null
@grep@ -Fx 'BASIC_TLV_LEN=32' @manifest@ >/dev/null
@grep@ -Fx 'INITIAL_BSS_PAYLOAD_LEN=36' @manifest@ >/dev/null
@grep@ -Fx 'INITIAL_BSS_COMMAND_LEN=84' @manifest@ >/dev/null
@grep@ -Fx 'ASSOCIATED_BSS_PAYLOAD_LEN=44' @manifest@ >/dev/null
@grep@ -Fx 'ASSOCIATED_BSS_COMMAND_LEN=92' @manifest@ >/dev/null
@grep@ -Fx 'QBSS_PAYLOAD_OFFSET=36' @manifest@ >/dev/null
@grep@ -Fx 'DTIM_SOURCE=selected-beacon-shared-basic-bcnft' @manifest@ >/dev/null
@grep@ -Fx 'INITIAL_BSS_COMMAND_SHA256=7aefeb7aa0e4eb196b676a1a5cb803cf287816abab430d6958021ffbf9cd273f' @manifest@ >/dev/null
@grep@ -Fx 'INITIAL_BSS_PAYLOAD_SHA256=c6dc7a127fef9e920c40eb43bc1a8495701eb1ce0bc0911a3f221aad456f0cde' @manifest@ >/dev/null
@grep@ -Fx 'ASSOCIATED_BSS_COMMAND_SHA256=6ea81837d7eb1aabe44edace8f8d8d280a60d48249fc2352e9a24a10390a9cc5' @manifest@ >/dev/null
@grep@ -Fx 'ASSOCIATED_BSS_PAYLOAD_SHA256=4d28837a85f136f2f2d34b2faad6aecee06798c84c4a21a72db89985f68aec8c' @manifest@ >/dev/null
@grep@ -Fx 'PASSIVE_M1_TELEMETRY_CONTRACT=linux-6.18.40-passive-m1-rx-v3' @manifest@ >/dev/null
@grep@ -Fx 'SAFE_READ_REGISTERS=0xd4208,0xd4528,0xd452c' @manifest@ >/dev/null
@grep@ -Fx 'CONSUMING_MIB_READS=false' @manifest@ >/dev/null
@grep@ -Fx 'SNAPSHOT_BOUNDARIES=before-post-assoc-tail,first-data-timeout-5000ms' @manifest@ >/dev/null
@grep@ -Fx 'POSITIVE_RESULT=target_m1_observed_at_rx_dma' @manifest@ >/dev/null
@grep@ -Fx 'NEGATIVE_RESULT=no_m1_at_rx_dma_ambiguous' @manifest@ >/dev/null
@grep@ -Fx 'TARGET_SCOPE=pinned-ap-to-client-exact-addr1-addr2-addr3-direction-and-eapol-key-m1' @manifest@ >/dev/null
@grep@ -Fx 'TELEMETRY_BEHAVIOR=best-effort-read-only-telemetry,control-flow-unchanged-except-observation-deadline-5000ms' @manifest@ >/dev/null
@grep@ -Fx 'ATTRIBUTION_LIMIT=independent-ap-or-over-air-witness-required' @manifest@ >/dev/null
@grep@ -F '"observation_mode":"passive-m1-observation"' @artifact_identity@ >/dev/null
@grep@ -F '"frame_tx_disabled_before_m1":true' @artifact_identity@ >/dev/null
@grep@ -F '"required_pre_m1_management_tx":"sae-and-association"' @artifact_identity@ >/dev/null
@grep@ -F '"preassociation_physical_tx_classes":"sae-authentication,association-request"' @artifact_identity@ >/dev/null
@grep@ -F '"postassociation_physical_tx":"disabled"' @artifact_identity@ >/dev/null
@grep@ -F '"management_tx_terminal_contract":"acked-txs+successful-tx-free;drop-retires;timeout-poisons"' @artifact_identity@ >/dev/null
@grep@ -F '"post_assoc_public_tx":"disabled-until-m1-observed"' @artifact_identity@ >/dev/null
@grep@ -F '"m2_physical_tx":"suppressed"' @artifact_identity@ >/dev/null
@grep@ -F '"frame":"none-post-association-public-before-m1"' @artifact_identity@ >/dev/null
@grep@ -Fx 'FD_CONTRACT=credential-fd3+snapshot-fd4+immediate-eof' @manifest@ >/dev/null
test "$(@launcher@ --artifact-identity)" = "$(@cat@ @artifact_identity@)"
assert_identity_field() {
  value=$(@sed@ -n "s/^$1=//p" @manifest@)
  test -n "$value"
  @grep@ -F "\"$2\":\"$value\"" @artifact_identity@ >/dev/null
}
assert_identity_field SOURCE_IDENTITY_SHA256 source_identity_sha256
assert_identity_field PROJECT_CORE_SOURCE_SHA256 project_core_source_sha256
assert_identity_field COMPOSITE_ARTIFACT_SOURCE_SHA256 composite_artifact_source_sha256
assert_identity_field FUCHSIA_BASE_REVISION fuchsia_base_revision
assert_identity_field FUCHSIA_ORDERED_PATCH_SET_SHA256 fuchsia_ordered_patch_set_sha256
assert_identity_field FUCHSIA_ORDERED_PATCH_LIST fuchsia_ordered_patch_list
assert_identity_field MATERIALIZED_SOURCE_TREE_SHA256 materialized_source_tree_sha256
assert_identity_field GENERATED_CRATE_SOURCE_SHA256 generated_crate_source_sha256
@grep@ -F '"artifact_identity":"mt7921-validation-v4"' @artifact_identity@ >/dev/null
@grep@ -F '"bss_wire_contract":"connac2-bss-wire-v1"' @artifact_identity@ >/dev/null
@grep@ -F '"basic_tlv_len":32' @artifact_identity@ >/dev/null
@grep@ -F '"initial_bss_payload_len":36,"initial_bss_command_len":84' @artifact_identity@ >/dev/null
@grep@ -F '"associated_bss_payload_len":44,"associated_bss_command_len":92' @artifact_identity@ >/dev/null
@grep@ -F '"qbss_payload_offset":36' @artifact_identity@ >/dev/null
@grep@ -F '"dtim_source":"selected-beacon-shared-basic-bcnft"' @artifact_identity@ >/dev/null
@grep@ -F '"initial_bss_command_sha256":"7aefeb7aa0e4eb196b676a1a5cb803cf287816abab430d6958021ffbf9cd273f"' @artifact_identity@ >/dev/null
@grep@ -F '"initial_bss_payload_sha256":"c6dc7a127fef9e920c40eb43bc1a8495701eb1ce0bc0911a3f221aad456f0cde"' @artifact_identity@ >/dev/null
@grep@ -F '"associated_bss_command_sha256":"6ea81837d7eb1aabe44edace8f8d8d280a60d48249fc2352e9a24a10390a9cc5"' @artifact_identity@ >/dev/null
@grep@ -F '"associated_bss_payload_sha256":"4d28837a85f136f2f2d34b2faad6aecee06798c84c4a21a72db89985f68aec8c"' @artifact_identity@ >/dev/null
@grep@ -F '"passive_m1_telemetry_contract":"linux-6.18.40-passive-m1-rx-v3"' @artifact_identity@ >/dev/null
@grep@ -F '"safe_read_registers":"0xd4208,0xd4528,0xd452c"' @artifact_identity@ >/dev/null
@grep@ -F '"consuming_mib_reads":false' @artifact_identity@ >/dev/null
@grep@ -F '"snapshot_boundaries":"before-post-assoc-tail,first-data-timeout-5000ms"' @artifact_identity@ >/dev/null
@grep@ -F '"positive_result":"target_m1_observed_at_rx_dma"' @artifact_identity@ >/dev/null
@grep@ -F '"negative_result":"no_m1_at_rx_dma_ambiguous"' @artifact_identity@ >/dev/null
@grep@ -F '"target_scope":"pinned-ap-to-client-exact-addr1-addr2-addr3-direction-and-eapol-key-m1"' @artifact_identity@ >/dev/null
@grep@ -F '"behavior":"best-effort-read-only-telemetry,control-flow-unchanged-except-observation-deadline-5000ms"' @artifact_identity@ >/dev/null
@grep@ -F '"attribution_limit":"independent-ap-or-over-air-witness-required"' @artifact_identity@ >/dev/null
fixture=$(@sed@ -n 's/^SAE_H2E_ASSOCIATION_REQUEST_SELF_TEST=//p' @manifest@)
fixture_sha=$(@sed@ -n 's/^SAE_H2E_ASSOCIATION_REQUEST_SELF_TEST_SHA256=//p' @manifest@)
test -s "$fixture"
test "$(@sha256sum@ "$fixture" | @cut@ -d ' ' -f1)" = "$fixture_sha"
printf 'ROOT_ENTRY privilege=sudo_-n manifest=%s manifest_sha256=%s supervisor=%s launcher=%s flavor=full-firmware-production active_capable=true bdf=0000:05:00.0 mode=%s\n' \
  @manifest@ "$(@sha256sum@ @manifest@ | @cut@ -d ' ' -f1)" \
  @supervisor@ @launcher@ "${operation:---active}"
if [ -n "$operation" ]; then
  exec @sudo@ -n @supervisor@ --plan 0000:05:00.0 -- @launcher@
fi
exec @sudo@ -n @supervisor@ 0000:05:00.0 -- @launcher@
