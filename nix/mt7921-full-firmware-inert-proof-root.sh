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
@grep@ -Fx 'ACTIVE_CAPABLE=true' "$manifest" >/dev/null
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
@grep@ -F '"active_capable":true' "$identity" >/dev/null

printf 'INERT_PROOF_ROOT privilege=sudo_-n runner=%s runner_sha256=%s runner_registered_hash=%s flavor=full-firmware-production operation=run-one-shot-sae-auth source_identity_sha256=@source_identity@ fuchsia_base_revision=@fuchsia_base_revision@ fuchsia_patch_set=@fuchsia_patch_set@ materialized_source_tree_sha256=@materialized_tree@ generated_crate_source_sha256=@generated_source@ active_capable=true mode=%s\n' \
  "$runner" @runner_sha256@ @runner_registered_hash@ "${operation:---execute}"
if [ -n "$operation" ]; then
  exec @sudo@ -n "$runner" --plan
fi
exec @sudo@ -n "$runner"
