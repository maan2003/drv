#!@shell@
set -eu

driver=@driver@
snapshot_generator=@snapshot_generator@
regulatory_db=@regulatory_db@
regulatory_source_sha256=@regulatory_source_sha256@
credential_file=@credential_file@
artifact_identity=@artifact_identity@

actual_identity=$($driver --artifact-identity)
expected_identity=$(@cat@ "$artifact_identity")
if [ "$actual_identity" != "$expected_identity" ]; then
  echo "installed validation ELF semantic identity mismatch" >&2
  exit 78
fi

prepare_snapshot() {
  umask 077
  snapshot_file=$(@mktemp@)
  trap '@rm@ -f "$snapshot_file"' EXIT
  "$snapshot_generator" --generate-regulatory-snapshot-v20 \
    "$regulatory_db" 00 "$regulatory_source_sha256" > "$snapshot_file"
  snapshot_len=$(@wc@ -c < "$snapshot_file")
  if [ "$snapshot_len" -lt 1 ] || [ "$snapshot_len" -gt 4096 ]; then
    echo "generated regulatory snapshot length is invalid" >&2
    exit 65
  fi
  exec 4< "$snapshot_file"
  @rm@ -f "$snapshot_file"
  trap - EXIT
}

prepare_credential() {
  umask 077
  credential=$(@sed@ -n 's/^Passphrase=\(.*\)$/\1/p' "$credential_file")
  credential_len=${#credential}
  if [ "$credential_len" -lt 8 ] || [ "$credential_len" -gt 63 ]; then
    echo "fixed SAE credential length is invalid" >&2
    exit 65
  fi
  credential_file_exact=$(@mktemp@)
  trap '@rm@ -f "$credential_file_exact"' EXIT
  printf '%s' "$credential" > "$credential_file_exact"
  unset credential
  test "$(@stat@ -c %a "$credential_file_exact")" = 600
  exec 3< "$credential_file_exact"
  @rm@ -f "$credential_file_exact"
  test ! -e "$credential_file_exact"
  trap - EXIT
}

case "$#:${1-}" in
  1:--artifact-identity)
    printf '%s\n' "$expected_identity"
    ;;
  0:)
    : "${DRV_PCI_BDF:?missing canonical PCI target}"
    : "${DRV_IOMMU_GROUP:?missing canonical IOMMU group}"
    : "${DRV_VFIO_DEVICE:?missing canonical VFIO device}"
    : "${DRV_LAB_SAFETY_STATE:?missing canonical lab safety state}"
    prepare_snapshot
    prepare_credential
    exec @env@ -i \
      DRV_PCI_BDF="$DRV_PCI_BDF" \
      DRV_IOMMU_GROUP="$DRV_IOMMU_GROUP" \
      DRV_VFIO_DEVICE="$DRV_VFIO_DEVICE" \
      DRV_LAB_SAFETY_STATE="$DRV_LAB_SAFETY_STATE" \
      DRV_SOCKS5_LISTEN="${DRV_SOCKS5_LISTEN-127.0.0.1:1080}" \
      DRV_DAEMON_MAX_SECONDS="${DRV_DAEMON_MAX_SECONDS-360}" \
      DRV_NETSTACK_BINARY=@netstack@ \
      DRV_ACTIVE_CLIENT=1 \
      DRV_SAE_BSSID=02:d3:b9:dd:c3:d0 \
      DRV_SAE_CHANNEL=149 \
      DRV_SAE_SSID=ajay \
      DRV_SAE_CLIENT_MAC=@session_client_mac@ \
      DRV_SAE_CREDENTIAL_FD=3 \
      DRV_SAE_CREDENTIAL_LEN="$credential_len" \
      DRV_REGULATORY_SNAPSHOT_FD=4 \
      DRV_REGULATORY_SNAPSHOT_LEN="$snapshot_len" \
      DRV_REGULATORY_SOURCE_SHA256="$regulatory_source_sha256" \
      "$driver" --run-one-shot-sae-auth
    ;;
  1:--full-firmware-preflight)
    prepare_snapshot
    prepare_credential
    exec @env@ -i \
      DRV_SAE_CREDENTIAL_FD=3 \
      DRV_SAE_CREDENTIAL_LEN="$credential_len" \
      DRV_REGULATORY_SNAPSHOT_FD=4 \
      DRV_REGULATORY_SNAPSHOT_LEN="$snapshot_len" \
      DRV_REGULATORY_SOURCE_SHA256="$regulatory_source_sha256" \
      "$driver" --full-firmware-preflight
    ;;
  *)
    echo "fixed full-firmware validation accepts no arguments except --artifact-identity or --full-firmware-preflight" >&2
    exit 64
    ;;
esac
