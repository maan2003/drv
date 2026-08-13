#!@shell@
set -eu

driver=@driver@
snapshot_generator=@snapshot_generator@
regulatory_db=@regulatory_db@
regulatory_source_sha256=@regulatory_source_sha256@
credential_file=@credential_file@

case "$#:${1-}" in
  0:)
    : "${DRV_PCI_BDF:?missing canonical PCI target}"
    : "${DRV_IOMMU_GROUP:?missing canonical IOMMU group}"
    : "${DRV_VFIO_DEVICE:?missing canonical VFIO device}"
    : "${DRV_LAB_SAFETY_STATE:?missing canonical lab safety state}"
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
    credential=$(@sed@ -n 's/^Passphrase=\(.*\)$/\1/p' "$credential_file")
    credential_len=${#credential}
    if [ "$credential_len" -lt 8 ] || [ "$credential_len" -gt 63 ]; then
      echo "fixed SAE credential length is invalid" >&2
      exit 65
    fi
    exec 3<<EOF_CREDENTIAL
$credential
EOF_CREDENTIAL
    unset credential
    exec @env@ -i \
      DRV_PCI_BDF="$DRV_PCI_BDF" \
      DRV_IOMMU_GROUP="$DRV_IOMMU_GROUP" \
      DRV_VFIO_DEVICE="$DRV_VFIO_DEVICE" \
      DRV_LAB_SAFETY_STATE="$DRV_LAB_SAFETY_STATE" \
      DRV_E2E94_EDCA_PROBE=1 \
      DRV_SAE_BSSID=72:a6:c7:7d:56:93 \
      DRV_SAE_CHANNEL=36 \
      DRV_SAE_SSID=ph1 \
      DRV_SAE_CLIENT_MAC=8a:fd:2a:8b:70:5a \
      DRV_SAE_CREDENTIAL_FD=3 \
      DRV_SAE_CREDENTIAL_LEN="$credential_len" \
      DRV_REGULATORY_SNAPSHOT_FD=4 \
      DRV_REGULATORY_SNAPSHOT_LEN="$snapshot_len" \
      DRV_REGULATORY_SOURCE_SHA256="$regulatory_source_sha256" \
      "$driver" --run-one-shot-sae-auth
    ;;
  1:--full-firmware-preflight)
    exec @env@ -i "$driver" --full-firmware-preflight
    ;;
  *)
    echo "fixed full-firmware validation accepts no arguments except --full-firmware-preflight" >&2
    exit 64
    ;;
esac
