#!/bin/sh

# Run only inside the supervised Redwood handoff described by
# ARCH-redwood-wifi-target. This wrapper invokes ath11k-bringup; it does not
# bind/unbind devices, arm watchdogs, or execute kexec itself.

set -eu

usage() {
    echo "usage: $0 [--dry-run] <ath11k-bringup-binary> <new-log-directory> <vfio-cdev>" >&2
    exit 2
}

quote() {
    printf "'"
    printf '%s' "$1" | sed "s/'/'\\\\''/g"
    printf "'"
}

print_command() {
    first=true
    for argument do
        if "$first"; then
            first=false
        else
            printf ' '
        fi
        quote "$argument"
    done
}

dry_run=false
if [ "${1-}" = "--dry-run" ]; then
    dry_run=true
    shift
fi
[ "$#" -eq 3 ] || usage

bringup=$1
log_dir=$2
vfio_cdev=$3

case $bringup in
    /*) ;;
    *) echo "ath11k-bringup binary must be an absolute path" >&2; exit 2 ;;
esac

native_trace=artifacts/redwood-native-ath11k/20260908T093708Z/wmi/ordered.jsonl

if "$dry_run"; then
    print_command mkdir "$log_dir"
    printf '\n'
    print_command "$bringup" preflight --vfio-device "$vfio_cdev"
    printf ' > '
    quote "$log_dir/preflight.log"
    printf ' 2>&1\n'
    for stage in resources firmware qmi core passive-scan scan-results; do
        print_command :
        printf ' > '
        quote "$log_dir/$stage.wmi.jsonl"
        printf '\n'
        print_command "$bringup" --vfio-device "$vfio_cdev" \
            --stop-after "$stage" --wmi-log "$log_dir/$stage.wmi.jsonl"
        printf ' > '
        quote "$log_dir/$stage.log"
        printf ' 2>&1\n'
    done
    printf '(cd '
    quote "$log_dir"
    printf ' && sha256sum *.log *.jsonl > SHA256SUMS)\n'
    printf '\nCompare the final stage with:\n'
    print_command cargo run -p ath11k-wmi --bin compare-wmi -- \
        "$native_trace" "$log_dir/scan-results.wmi.jsonl"
    printf '\n'
    exit 0
fi

[ -x "$bringup" ] || { echo "ath11k-bringup binary is not executable: $bringup" >&2; exit 2; }
mkdir "$log_dir" || {
    echo "log directory must be new and creatable: $log_dir" >&2
    exit 2
}

write_sums() {
    sums_tmp=$log_dir/.SHA256SUMS.tmp
    (
        cd "$log_dir"
        for file in *.log *.jsonl; do
            [ -f "$file" ] || continue
            sha256sum "$file"
        done
    ) > "$sums_tmp"
    mv "$sums_tmp" "$log_dir/SHA256SUMS"
}

if ! "$bringup" preflight --vfio-device "$vfio_cdev" > "$log_dir/preflight.log" 2>&1; then
    write_sums
    echo "preflight failed; refusing all stages (see $log_dir/preflight.log)" >&2
    exit 1
fi
[ -f "$log_dir/preflight.log" ] || {
    echo "preflight log is missing; refusing resources stage" >&2
    exit 1
}

previous_log=$log_dir/preflight.log
previous_wmi=
for stage in resources firmware qmi core passive-scan scan-results; do
    [ -f "$previous_log" ] || {
        write_sums
        echo "previous stage log is missing; refusing $stage stage" >&2
        exit 1
    }
    if [ -n "$previous_wmi" ] && [ ! -f "$previous_wmi" ]; then
        write_sums
        echo "previous stage WMI log is missing; refusing $stage stage" >&2
        exit 1
    fi

    stage_log=$log_dir/$stage.log
    stage_wmi=$log_dir/$stage.wmi.jsonl
    : > "$stage_wmi"
    if ! "$bringup" --vfio-device "$vfio_cdev" \
        --stop-after "$stage" --wmi-log "$stage_wmi" > "$stage_log" 2>&1; then
        write_sums
        echo "$stage stage failed; refusing later stages (see $stage_log)" >&2
        exit 1
    fi
    [ -f "$stage_log" ] && [ -f "$stage_wmi" ] || {
        write_sums
        echo "$stage stage log is missing; refusing later stages" >&2
        exit 1
    }
    previous_log=$stage_log
    previous_wmi=$stage_wmi
done

write_sums
printf 'All six stages completed. SHA-256 manifest: %s\n' "$log_dir/SHA256SUMS"
printf '\nCompare the final stage with:\n'
print_command cargo run -p ath11k-wmi --bin compare-wmi -- \
    "$native_trace" "$log_dir/scan-results.wmi.jsonl"
printf '\n'
