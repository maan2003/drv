#!@shell@
set -eu
case "$#:${1-}" in
  0:) operation= ;;
  1:--plan) operation=--plan ;;
  *) echo "fixed production root entrypoint accepts no arguments except --plan" >&2; exit 64 ;;
esac
@sha256sum@ @manifest@ >/dev/null
@grep@ -Fx 'FLAVOR=@flavor@' @manifest@ >/dev/null
@grep@ -Fx 'ACTIVE_CAPABLE=true' @manifest@ >/dev/null
@grep@ -Fx 'BSS_WIRE_CONTRACT=connac2-bss-wire-v1' @manifest@ >/dev/null
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
@grep@ -F '"artifact_identity":"mt7921-driver-v11"' @artifact_identity@ >/dev/null
@grep@ -F '"bss_wire_contract":"connac2-bss-wire-v1"' @artifact_identity@ >/dev/null
@grep@ -F '"initial_bss_command_sha256":"7aefeb7aa0e4eb196b676a1a5cb803cf287816abab430d6958021ffbf9cd273f"' @artifact_identity@ >/dev/null
@grep@ -F '"associated_bss_command_sha256":"6ea81837d7eb1aabe44edace8f8d8d280a60d48249fc2352e9a24a10390a9cc5"' @artifact_identity@ >/dev/null
printf 'ROOT_ENTRY privilege=sudo_-n manifest=%s supervisor=%s launcher=%s flavor=@flavor@ active_capable=true mode=%s\n' \
  @manifest@ @supervisor@ @launcher@ "${operation:---active}"
if [ -n "$operation" ]; then exec @sudo@ -n @supervisor@ --plan 0000:05:00.0 -- @launcher@; fi
exec @sudo@ -n @supervisor@ 0000:05:00.0 -- @launcher@
