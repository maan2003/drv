#!@shell@
set -eu

case "$#:${1-}" in
  0:) operation= ;;
  1:--plan) operation=--plan ;;
  *)
    echo "fixed inert-proof root entrypoint accepts no arguments except --plan" >&2
    exit 64
    ;;
esac

runner=@runner@
runner_package=@runner_package@
manifest=@manifest@
launcher=@launcher@
identity=@artifact_identity@

if [ ! -x @sudo@ ]; then
  echo "fixed inert-proof root entrypoint requires noninteractive sudo" >&2
  exit 77
fi
@sha256sum@ "$manifest" >/dev/null
@grep@ -Fx "RUNNER=$runner" "$manifest" >/dev/null
@grep@ -Fx "RUNNER_SHA256=@runner_sha256@" "$manifest" >/dev/null
@grep@ -Fx "RUNNER_REGISTERED_HASH=@runner_registered_hash@" "$manifest" >/dev/null
@grep@ -Fx 'FLAVOR=full-firmware-production' "$manifest" >/dev/null
@grep@ -Fx 'OPERATION=run-one-shot-sae-auth' "$manifest" >/dev/null
@grep@ -Fx 'SOURCE_IDENTITY_SHA256=@source_identity@' "$manifest" >/dev/null
@grep@ -Fx 'FUCHSIA_BASE_REVISION=@fuchsia_base_revision@' "$manifest" >/dev/null
@grep@ -Fx 'FUCHSIA_ORDERED_PATCH_SET_SHA256=@fuchsia_patch_set@' "$manifest" >/dev/null
@grep@ -Fx 'FUCHSIA_ORDERED_PATCH_LIST=@fuchsia_patch_list@' "$manifest" >/dev/null
@grep@ -Fx 'MATERIALIZED_SOURCE_TREE_SHA256=@materialized_tree@' "$manifest" >/dev/null
@grep@ -Fx 'GENERATED_CRATE_SOURCE_SHA256=@generated_source@' "$manifest" >/dev/null
@grep@ -Fx 'PROJECT_CORE_SOURCE_SHA256=@project_core@' "$manifest" >/dev/null
@grep@ -Fx 'COMPOSITE_ARTIFACT_SOURCE_SHA256=@composite_source@' "$manifest" >/dev/null
@grep@ -Fx 'BSS_WIRE_CONTRACT=connac2-bss-wire-v1' "$manifest" >/dev/null
@grep@ -Fx 'BASIC_TLV_LEN=32' "$manifest" >/dev/null
@grep@ -Fx 'INITIAL_BSS_PAYLOAD_LEN=36' "$manifest" >/dev/null
@grep@ -Fx 'INITIAL_BSS_COMMAND_LEN=84' "$manifest" >/dev/null
@grep@ -Fx 'ASSOCIATED_BSS_PAYLOAD_LEN=44' "$manifest" >/dev/null
@grep@ -Fx 'ASSOCIATED_BSS_COMMAND_LEN=92' "$manifest" >/dev/null
@grep@ -Fx 'QBSS_PAYLOAD_OFFSET=36' "$manifest" >/dev/null
@grep@ -Fx 'DTIM_SOURCE=selected-beacon-shared-basic-bcnft' "$manifest" >/dev/null
@grep@ -Fx 'INITIAL_BSS_COMMAND_SHA256=7aefeb7aa0e4eb196b676a1a5cb803cf287816abab430d6958021ffbf9cd273f' "$manifest" >/dev/null
@grep@ -Fx 'INITIAL_BSS_PAYLOAD_SHA256=c6dc7a127fef9e920c40eb43bc1a8495701eb1ce0bc0911a3f221aad456f0cde' "$manifest" >/dev/null
@grep@ -Fx 'ASSOCIATED_BSS_COMMAND_SHA256=6ea81837d7eb1aabe44edace8f8d8d280a60d48249fc2352e9a24a10390a9cc5' "$manifest" >/dev/null
@grep@ -Fx 'ASSOCIATED_BSS_PAYLOAD_SHA256=4d28837a85f136f2f2d34b2faad6aecee06798c84c4a21a72db89985f68aec8c' "$manifest" >/dev/null
@grep@ -Fx 'ASSOCIATION_REQUEST_CONTRACT=mt7921-supported-subset-v2' "$manifest" >/dev/null
@grep@ -Fx 'CANONICAL_ASSOCIATION_FIXTURE_SHA256=5449fa5acf5317259694bb400a04d6ba8e169f99cf555583a424b3530f8a63c4' "$manifest" >/dev/null
@grep@ -Fx 'RUNTIME_ASSOCIATION_HASH_POLICY=input-dependent' "$manifest" >/dev/null
@grep@ -Fx 'ASSOCIATION_CAPABILITY_INPUT_SOURCE=firmware-nic-capability+pinned-regdb-to-softmac-query-band-v2' "$manifest" >/dev/null
@grep@ -Fx 'ASSOCIATION_TRANSFORMATION_CONTRACT=device+pinned-regdb-authoritative-association-v2' "$manifest" >/dev/null
@grep@ -Fx 'ORACLE_COMPARISON_CONTRACT=linux-6.18.40-semantic-v1' @manifest@ >/dev/null
@grep@ -Fx 'ORACLE_COMPARISON_NORMALIZED_SHA256=6a80b1b8631d70447f20b1be45a35564a806bc8913848d9fdb51c3404ddf4755' @manifest@ >/dev/null
@grep@ -Fx 'PASSIVE_M1_TELEMETRY_CONTRACT=linux-6.18.40-passive-m1-rx-v8' "$manifest" >/dev/null
@grep@ -Fx 'EARLY_M1_LATCH_CONTRACT=exact-m1-one-frame-epoch-v1' "$manifest" >/dev/null
@grep@ -Fx 'EARLY_M1_DUPLICATE_POLICY=same-replay-and-byte-identical-complete-frame-ignore;changed-byte-or-replay-poisons-containment' "$manifest" >/dev/null
@grep@ -Fx 'SAFE_READ_REGISTERS=0xd4208,0xd4528,0xd452c,0x820d8108,0x820e5000,0x820e5004' "$manifest" >/dev/null
@grep@ -Fx 'CONSUMING_MIB_READS=false' "$manifest" >/dev/null
@grep@ -Fx 'SNAPSHOT_BOUNDARIES=before-associated-bss-sta-edca,after-pre-associated-rx-pump-15ms,m1-observation-timeout-5000ms' "$manifest" >/dev/null
@grep@ -Fx 'POSITIVE_RESULT=target_m1_observed_at_rx_dma' "$manifest" >/dev/null
@grep@ -Fx 'NEGATIVE_RESULT=no_m1_at_rx_dma_ambiguous' "$manifest" >/dev/null
@grep@ -Fx 'TARGET_SCOPE=pinned-ap-to-client-exact-addr1-addr2-addr3-direction-and-eapol-key-m1' "$manifest" >/dev/null
@grep@ -Fx 'TELEMETRY_BEHAVIOR=best-effort-read-only-telemetry,observer-deadline-5000ms,validation-only-initial-rsna-response-timeout-6000ms,normal-mode-timeouts-unchanged' "$manifest" >/dev/null
@grep@ -Fx 'ATTRIBUTION_LIMIT=independent-ap-or-over-air-witness-required' "$manifest" >/dev/null
@grep@ -Fx 'TARGET_BEACON_TIM_CONTRACT=linux-ieee80211-check-tim-v1' "$manifest" >/dev/null
@grep@ -Fx 'TIM_TRUE_RESULT=ap-queued-unicast-for-normalized-aid-not-traffic-type' "$manifest" >/dev/null
@grep@ -Fx 'TIM_NEVER_TRUE_RESULT=inconclusive' "$manifest" >/dev/null
@grep@ -Fx 'ACTIVE_CAPABLE=true' "$manifest" >/dev/null
@grep@ -Fx 'OBSERVATION_MODE=passive-m1-observation' "$manifest" >/dev/null
@grep@ -Fx 'FRAME_TX_DISABLED_BEFORE_M1=true' "$manifest" >/dev/null
@grep@ -Fx 'REQUIRED_PRE_M1_MANAGEMENT_TX=sae-and-association' "$manifest" >/dev/null
@grep@ -Fx 'PREASSOCIATION_PHYSICAL_TX_CLASSES=sae-authentication,association-request' "$manifest" >/dev/null
@grep@ -Fx 'POSTASSOCIATION_PHYSICAL_TX=disabled' "$manifest" >/dev/null
@grep@ -Fx 'MANAGEMENT_TX_TERMINAL_CONTRACT=acked-txs+successful-tx-free;drop-retires;timeout-poisons' "$manifest" >/dev/null
@grep@ -Fx 'POST_ASSOC_PUBLIC_TX=disabled-until-m1-observed' "$manifest" >/dev/null
@grep@ -Fx 'M2_PHYSICAL_TX=suppressed' "$manifest" >/dev/null
test "$(@sha256sum@ "$runner" | @cut@ -d ' ' -f1)" = @runner_sha256@
test "$(@nix_store@ -q --hash "$runner_package")" = @runner_registered_hash@
test "$(@sha256sum@ "$0" | @cut@ -d ' ' -f1)" = "$(@sed@ -n 's/^ENTRYPOINT_SHA256=//p' "$manifest")"
test "$($launcher --artifact-identity)" = "$(@cat@ "$identity")"
@grep@ -F '"flavor":"full-firmware-production"' "$identity" >/dev/null
@grep@ -F '"enabled_operation":"run-one-shot-sae-auth"' "$identity" >/dev/null
@grep@ -F '"source_identity_sha256":"@source_identity@"' "$identity" >/dev/null
@grep@ -F '"fuchsia_base_revision":"@fuchsia_base_revision@"' "$identity" >/dev/null
@grep@ -F '"fuchsia_ordered_patch_set_sha256":"@fuchsia_patch_set@"' "$identity" >/dev/null
@grep@ -F '"fuchsia_ordered_patch_list":"@fuchsia_patch_list@"' "$identity" >/dev/null
@grep@ -F '"materialized_source_tree_sha256":"@materialized_tree@"' "$identity" >/dev/null
@grep@ -F '"generated_crate_source_sha256":"@generated_source@"' "$identity" >/dev/null
@grep@ -F '"project_core_source_sha256":"@project_core@"' "$identity" >/dev/null
@grep@ -F '"composite_artifact_source_sha256":"@composite_source@"' "$identity" >/dev/null
@grep@ -F '"artifact_identity":"mt7921-validation-v8"' "$identity" >/dev/null
@grep@ -F '"bss_wire_contract":"connac2-bss-wire-v1"' "$identity" >/dev/null
@grep@ -F '"basic_tlv_len":32' "$identity" >/dev/null
@grep@ -F '"initial_bss_payload_len":36,"initial_bss_command_len":84' "$identity" >/dev/null
@grep@ -F '"associated_bss_payload_len":44,"associated_bss_command_len":92' "$identity" >/dev/null
@grep@ -F '"qbss_payload_offset":36' "$identity" >/dev/null
@grep@ -F '"dtim_source":"selected-beacon-shared-basic-bcnft"' "$identity" >/dev/null
@grep@ -F '"initial_bss_command_sha256":"7aefeb7aa0e4eb196b676a1a5cb803cf287816abab430d6958021ffbf9cd273f"' "$identity" >/dev/null
@grep@ -F '"initial_bss_payload_sha256":"c6dc7a127fef9e920c40eb43bc1a8495701eb1ce0bc0911a3f221aad456f0cde"' "$identity" >/dev/null
@grep@ -F '"associated_bss_command_sha256":"6ea81837d7eb1aabe44edace8f8d8d280a60d48249fc2352e9a24a10390a9cc5"' "$identity" >/dev/null
@grep@ -F '"associated_bss_payload_sha256":"4d28837a85f136f2f2d34b2faad6aecee06798c84c4a21a72db89985f68aec8c"' "$identity" >/dev/null
@grep@ -F '"association_request_contract":"mt7921-supported-subset-v2"' "$identity" >/dev/null
@grep@ -F '"canonical_association_fixture_sha256":"5449fa5acf5317259694bb400a04d6ba8e169f99cf555583a424b3530f8a63c4"' "$identity" >/dev/null
@grep@ -F '"runtime_association_hash_policy":"input-dependent"' "$identity" >/dev/null
@grep@ -F '"association_capability_input_source":"firmware-nic-capability+pinned-regdb-to-softmac-query-band-v2"' "$identity" >/dev/null
@grep@ -F '"association_transformation_contract":"device+pinned-regdb-authoritative-association-v2"' "$identity" >/dev/null
@grep@ -F '"oracle_comparison_contract":"linux-6.18.40-semantic-v1"' "$identity" >/dev/null
@grep@ -F '"oracle_comparison_normalized_sha256":"6a80b1b8631d70447f20b1be45a35564a806bc8913848d9fdb51c3404ddf4755"' "$identity" >/dev/null
@grep@ -F '"passive_m1_telemetry_contract":"linux-6.18.40-passive-m1-rx-v8"' "$identity" >/dev/null
@grep@ -F '"early_m1_latch_contract":"exact-m1-one-frame-epoch-v1"' "$identity" >/dev/null
@grep@ -F '"early_m1_duplicate_policy":"same-replay-and-byte-identical-complete-frame-ignore;changed-byte-or-replay-poisons-containment"' "$identity" >/dev/null
@grep@ -F '"safe_read_registers":"0xd4208,0xd4528,0xd452c,0x820d8108,0x820e5000,0x820e5004"' "$identity" >/dev/null
@grep@ -F '"consuming_mib_reads":false' "$identity" >/dev/null
@grep@ -F '"snapshot_boundaries":"before-associated-bss-sta-edca,after-pre-associated-rx-pump-15ms,m1-observation-timeout-5000ms"' "$identity" >/dev/null
@grep@ -F '"positive_result":"target_m1_observed_at_rx_dma"' "$identity" >/dev/null
@grep@ -F '"negative_result":"no_m1_at_rx_dma_ambiguous"' "$identity" >/dev/null
@grep@ -F '"target_scope":"pinned-ap-to-client-exact-addr1-addr2-addr3-direction-and-eapol-key-m1"' "$identity" >/dev/null
@grep@ -F '"behavior":"best-effort-read-only-telemetry,observer-deadline-5000ms,validation-only-initial-rsna-response-timeout-6000ms,normal-mode-timeouts-unchanged"' "$identity" >/dev/null
@grep@ -F '"attribution_limit":"independent-ap-or-over-air-witness-required","target_beacon_tim_contract":"linux-ieee80211-check-tim-v1","tim_true_result":"ap-queued-unicast-for-normalized-aid-not-traffic-type","tim_never_true_result":"inconclusive"' "$identity" >/dev/null
@grep@ -F '"active_capable":true' "$identity" >/dev/null
@grep@ -F '"observation_mode":"passive-m1-observation"' "$identity" >/dev/null
@grep@ -F '"frame_tx_disabled_before_m1":true' "$identity" >/dev/null
@grep@ -F '"required_pre_m1_management_tx":"sae-and-association"' "$identity" >/dev/null
@grep@ -F '"preassociation_physical_tx_classes":"sae-authentication,association-request"' "$identity" >/dev/null
@grep@ -F '"postassociation_physical_tx":"disabled"' "$identity" >/dev/null
@grep@ -F '"management_tx_terminal_contract":"acked-txs+successful-tx-free;drop-retires;timeout-poisons"' "$identity" >/dev/null
@grep@ -F '"post_assoc_public_tx":"disabled-until-m1-observed"' "$identity" >/dev/null
@grep@ -F '"m2_physical_tx":"suppressed"' "$identity" >/dev/null
@grep@ -F '"frame":"none-post-association-public-before-m1"' "$identity" >/dev/null

printf 'INERT_PROOF_ROOT privilege=sudo_-n runner=%s runner_sha256=%s runner_registered_hash=%s flavor=full-firmware-production operation=run-one-shot-sae-auth source_identity_sha256=@source_identity@ fuchsia_base_revision=@fuchsia_base_revision@ fuchsia_patch_set=@fuchsia_patch_set@ materialized_source_tree_sha256=@materialized_tree@ generated_crate_source_sha256=@generated_source@ active_capable=true mode=%s\n' \
  "$runner" @runner_sha256@ @runner_registered_hash@ "${operation:---execute}"
if [ -n "$operation" ]; then
  exec @sudo@ -n "$runner" --plan
fi
exec @sudo@ -n "$runner"
