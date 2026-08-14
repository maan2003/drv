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
@grep@ -Fx 'POST_ASSOC_PUBLIC_TX=disabled-until-m1-observed' @manifest@ >/dev/null
@grep@ -Fx 'M2_PHYSICAL_TX=suppressed' @manifest@ >/dev/null
@grep@ -F '"observation_mode":"passive-m1-observation"' @artifact_identity@ >/dev/null
@grep@ -F '"frame_tx_disabled_before_m1":true' @artifact_identity@ >/dev/null
@grep@ -F '"required_pre_m1_management_tx":"sae-and-association"' @artifact_identity@ >/dev/null
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
assert_identity_field FUCHSIA_BASE_REVISION fuchsia_base_revision
assert_identity_field FUCHSIA_ORDERED_PATCH_SET_SHA256 fuchsia_ordered_patch_set_sha256
assert_identity_field FUCHSIA_ORDERED_PATCH_LIST fuchsia_ordered_patch_list
assert_identity_field MATERIALIZED_SOURCE_TREE_SHA256 materialized_source_tree_sha256
assert_identity_field GENERATED_CRATE_SOURCE_SHA256 generated_crate_source_sha256
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
