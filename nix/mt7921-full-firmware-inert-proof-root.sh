#!@shell@
set -eu
case "$#:${1-}" in
  0:) operation= ;;
  1:--plan) operation=--plan ;;
  *) echo "fixed inert-proof root entrypoint accepts no arguments except --plan" >&2; exit 64 ;;
esac
runner=@runner@
manifest=@manifest@
launcher=@launcher@
identity=@artifact_identity@
if [ ! -x @sudo@ ]; then echo "fixed inert-proof root entrypoint requires noninteractive sudo" >&2; exit 77; fi
@sha256sum@ "$manifest" >/dev/null
@grep@ -Fx "RUNNER=$runner" "$manifest" >/dev/null
@grep@ -Fx 'FLAVOR=full-firmware-production' "$manifest" >/dev/null
@grep@ -Fx 'SOURCE_IDENTITY_SHA256=@source_identity@' "$manifest" >/dev/null
test "$($launcher --artifact-identity)" = "$(@cat@ "$identity")"
@grep@ -F '"artifact_identity":"mt7921-driver-v11"' "$identity" >/dev/null
@grep@ -F '"bss_wire_contract":"connac2-bss-wire-v1"' "$identity" >/dev/null
@grep@ -F '"active_capable":true' "$identity" >/dev/null
printf 'INERT_PROOF_ROOT privilege=sudo_-n runner=%s flavor=full-firmware-production source_identity_sha256=@source_identity@ mode=%s\n' \
  "$runner" "${operation:---execute}"
if [ -n "$operation" ]; then exec @sudo@ -n "$runner" --plan; fi
exec @sudo@ -n "$runner"
