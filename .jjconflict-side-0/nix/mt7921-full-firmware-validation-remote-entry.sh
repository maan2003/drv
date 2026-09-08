#!@shell@
set -euo pipefail

case "$#:${1-}" in
  0:) requested=active ;;
  1:--plan) requested=plan ;;
  *)
    echo "fixed remote entrypoint accepts no arguments except --plan" >&2
    exit 64
    ;;
esac

if [ "${HOME-}" != @home@ ]; then
  echo "HOME does not match the fixed transport contract" >&2
  exit 66
fi
key=@home@/.ssh/no-plastic
known_hosts=@home@/.ssh/known_hosts
if [ "${XDG_RUNTIME_DIR-}" != @xdg_runtime_dir@ ]; then
  echo "XDG_RUNTIME_DIR does not match the fixed transport contract" >&2
  exit 66
fi
socket=@xdg_runtime_dir@/tailscale/tailscaled.sock
if [ ! -f "$key" ]; then
  echo "fixed SSH key is missing: $key" >&2
  exit 66
fi
if [ ! -f "$known_hosts" ]; then
  echo "fixed SSH known_hosts is missing: $known_hosts" >&2
  exit 66
fi
if [ ! -S "$socket" ]; then
  echo "fixed userspace Tailscale socket is missing: $socket" >&2
  exit 66
fi

target_root=@target_root@
target_entry=$target_root/bin/mt7921-full-firmware-validation-root
target_package=@target_package@
target_manifest=@target_manifest@
target_supervisor=@target_supervisor@
target_supervisor_file=$target_supervisor/bin/mt7921-full-firmware-validation-supervisor
target_identity=$target_package/share/mt7921-full-firmware-validation/artifact-identity.json
target_launcher=$target_package/bin/mt7921-full-firmware-validation
target_nix_store=@target_nix_store@
target_sha256sum=@target_sha256sum@
target_recovery_helper=@target_recovery_helper@
target_recovery_package=@target_recovery_package@
target_sudo=@target_sudo@

ssh_argv=(
  @ssh@
  -F /dev/null
  -i "$key"
  -o BatchMode=yes
  -o IdentitiesOnly=yes
  -o ConnectTimeout=10
  -o ServerAliveInterval=2
  -o ServerAliveCountMax=3
  -o StrictHostKeyChecking=yes
  -o "UserKnownHostsFile=$known_hosts"
  -o GlobalKnownHostsFile=/dev/null
  -o "ProxyCommand=@tailscale@ --socket=$socket nc %h %p"
  user@no-plastic
)

remote() {
  "${ssh_argv[@]}" "$@"
}

remote_recovery() {
  @timeout@ @recovery_call_timeout_seconds@ "${ssh_argv[@]}" \
    "$target_sudo" -n "$target_recovery_helper" "$@"
}

verify_registered() {
  local path=$1 expected=$2 actual
  remote "$target_nix_store" --verify-path "$path" || {
    echo "remote store content verification failed: $path" >&2
    return 1
  }
  actual=$(remote "$target_nix_store" -q --hash "$path") || {
    echo "remote registered-hash verification failed: $path" >&2
    return 1
  }
  if [ "$actual" != "$expected" ]; then
    echo "remote registered hash mismatch: $path" >&2
    return 1
  fi
  printf 'REMOTE_REGISTERED_HASH path=%s hash=%s\n' "$path" "$actual"
}

verify_file() {
  local path=$1 expected=$2 actual
  actual=$(remote "$target_sha256sum" "$path") || {
    echo "remote file-hash verification failed: $path" >&2
    return 1
  }
  if [ "$actual" != "$expected  $path" ]; then
    echo "remote file hash mismatch: $path" >&2
    return 1
  fi
  printf 'REMOTE_FILE_SHA256 path=%s sha256=%s\n' "$path" "$expected"
}

verify_remote_contract() {
  local plan recovery_version recovery_rc
  verify_registered "$target_root" @target_root_registered_hash@
  verify_registered "$target_package" @target_package_registered_hash@
  verify_registered "$target_manifest" @target_manifest_registered_hash@
  verify_registered "$target_supervisor" @target_supervisor_registered_hash@
  verify_registered "$target_recovery_package" @target_recovery_registered_hash@
  verify_file "$target_entry" @target_entry_sha256@
  verify_file "$target_manifest" @target_manifest_sha256@
  verify_file "$target_supervisor_file" @target_supervisor_sha256@
  verify_file "$target_identity" @target_identity_sha256@
  verify_file "$target_launcher" @target_launcher_sha256@
  verify_file "$target_recovery_helper" @target_recovery_sha256@
  recovery_version=$(remote_recovery --version) || {
    echo "fixed recovery status version query failed" >&2
    return 1
  }
  [ "$recovery_version" = mt7921-full-firmware-recovery-status-v1 ] || {
    echo "fixed recovery status version mismatch" >&2
    return 1
  }
  remote_recovery --idle >/dev/null || {
    echo "target is not idle before handoff" >&2
    return 1
  }
  set +e
  remote_recovery --quarantined >/dev/null
  recovery_rc=$?
  set -e
  [ "$recovery_rc" -eq 1 ] || {
    echo "target quarantine status is unsafe or invalid before handoff" >&2
    return 1
  }
  remote_recovery --native-ready 0000:05:00.0 >/dev/null || {
    echo "target native-ready contract failed before handoff" >&2
    return 1
  }
  printf 'REMOTE_RECOVERY_STATUS contract=%s package=%s helper=%s helper_sha256=%s registered_hash=%s idle=true quarantined=false native_ready=true\n' \
    "$recovery_version" "$target_recovery_package" "$target_recovery_helper" \
    @target_recovery_sha256@ @target_recovery_registered_hash@
  plan=$(remote "$target_entry" --plan) || {
    echo "fixed target root plan failed" >&2
    return 1
  }
  @grep@ -F "ROOT_ENTRY privilege=sudo_-n manifest=$target_manifest manifest_sha256=@target_manifest_sha256@ supervisor=$target_supervisor_file launcher=$target_launcher flavor=full-firmware-production active_capable=true bdf=0000:05:00.0 mode=--plan" <<<"$plan" >/dev/null
  @grep@ -F 'PLAN mode=inert hardware_handoff=false' <<<"$plan" >/dev/null
  @grep@ -F "supervisor=$target_supervisor_file supervisor_sha256=@target_supervisor_sha256@" <<<"$plan" >/dev/null
  @grep@ -F "launcher=$target_launcher launcher_sha256=@target_launcher_sha256@" <<<"$plan" >/dev/null
  printf '%s\n' "$plan"
}

print_active_argv() {
  printf 'REMOTE_ACTIVE_ARGV'
  printf ' <%s>' "${ssh_argv[@]}" "$target_entry"
  printf '\n'
}

# This preflight is deliberately shared by plan and active operation.  It
# rejects an unknown host key or artifact drift before the hardware handoff.
verify_remote_contract
print_active_argv
printf 'REMOTE_TRANSPORT contract=@transport_contract@ target=user@no-plastic recovery=poll-only-bounded-no-disarm\n'
if [ "$requested" = plan ]; then
  printf 'REMOTE_PLAN hardware_handoff=false watchdog_operation=false\n'
  exit 0
fi

set +e
remote "$target_entry"
experiment_rc=$?
set -e
if [ "$experiment_rc" -ne 255 ]; then
  exit "$experiment_rc"
fi

echo "active SSH status unknown; target durable supervisor/report is authoritative" >&2
attempt=0
while [ "$attempt" -lt 60 ]; do
  attempt=$((attempt + 1))
  set +e
  remote_recovery --quarantined >/dev/null
  quarantined_rc=$?
  set -e
  case "$quarantined_rc" in
    0)
      echo "target reports quarantine; no watchdog ownership action taken" >&2
      exit 75
      ;;
    1) ;;
    124|255)
      @sleep@ 5
      continue
      ;;
    *)
      echo "target quarantine status contract failed" >&2
      exit 75
      ;;
  esac

  set +e
  remote_recovery --idle >/dev/null
  idle_rc=$?
  set -e
  case "$idle_rc" in
    0)
      set +e
      remote_recovery --native-ready 0000:05:00.0 >/dev/null
      ready_rc=$?
      set -e
      if [ "$ready_rc" -eq 0 ]; then
        echo "target recovery is complete; experiment outcome remains unknown" >&2
        exit 75
      fi
      if [ "$ready_rc" -ne 1 ] && [ "$ready_rc" -ne 124 ] && [ "$ready_rc" -ne 255 ]; then
        echo "target native-ready status contract failed" >&2
        exit 75
      fi
      ;;
    1|124|255) ;;
    *)
      echo "target idle status contract failed" >&2
      exit 75
      ;;
  esac
  @sleep@ 5
done

echo "bounded target recovery-status polling expired; no watchdog ownership action taken" >&2
exit 75
