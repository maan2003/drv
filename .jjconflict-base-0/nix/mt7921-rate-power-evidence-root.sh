#!@shell@
set -eu

case "$#:${1-}" in
  0:)
    operation=
    ;;
  1:--plan)
    operation=--plan
    ;;
  *)
    echo "fixed root evidence entrypoint accepts no arguments except --plan" >&2
    exit 64
    ;;
esac

@sha256sum@ @manifest@ >/dev/null
printf 'ROOT_ENTRY privilege=sudo_-n manifest=%s manifest_sha256=%s supervisor=%s launcher=%s bdf=0000:05:00.0 mode=%s\n' \
  @manifest@ "$(@sha256sum@ @manifest@ | @cut@ -d ' ' -f1)" \
  @supervisor@ @launcher@ "${operation:---active}"
if [ -n "$operation" ]; then
  exec @sudo@ -n @supervisor@ --plan 0000:05:00.0 -- @launcher@
fi
exec @sudo@ -n @supervisor@ 0000:05:00.0 -- @launcher@
