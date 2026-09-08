#!@shell@
set -eu

case "$#:${1-}" in
  0:) operation= ;;
  1:--plan) operation=--plan ;;
  *) echo "fixed evidence root entrypoint accepts no arguments except --plan" >&2; exit 64 ;;
esac
@sha256sum@ @manifest@ >/dev/null
grep -Fx 'FLAVOR=rate-power-evidence-only' @manifest@ >/dev/null
grep -Fx 'ACTIVE_CAPABLE=false' @manifest@ >/dev/null
grep -Fx 'FD_CONTRACT=credential-fd3+snapshot-fd4+immediate-eof' @manifest@ >/dev/null
test "$(@launcher@ --artifact-identity)" = "$(cat @artifact_identity@)"
printf 'ROOT_ENTRY privilege=sudo_-n manifest=%s supervisor=%s launcher=%s flavor=rate-power-evidence-only active_capable=false mode=%s\n' \
  @manifest@ @supervisor@ @launcher@ "${operation:---active}"
if [ -n "$operation" ]; then exec @sudo@ -n @supervisor@ --plan 0000:05:00.0 -- @launcher@; fi
exec @sudo@ -n @supervisor@ 0000:05:00.0 -- @launcher@
