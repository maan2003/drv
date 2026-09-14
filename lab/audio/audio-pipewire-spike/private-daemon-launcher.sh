#!@shell@
set -u

runtime_dir="${DRV_AUDIO_RUNTIME_DIR:-${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR must be set}/drv-audio-pipewire}"
socket="$runtime_dir/pipewire-0"
mkdir -p "$runtime_dir"
chmod 0700 "$runtime_dir"
rm -f "$socket"

child=
stop() {
  trap - TERM INT EXIT
  if [[ -n "$child" ]] && kill -0 "$child" 2>/dev/null; then
    kill -TERM "$child" 2>/dev/null || true
    wait "$child" 2>/dev/null || true
  fi
  rm -f "$socket"
}
trap 'stop; exit 0' TERM INT
trap stop EXIT

PIPEWIRE_RUNTIME_DIR="$runtime_dir" @daemon@ daemon &
child=$!
wait "$child"
status=$?
child=
exit "$status"
