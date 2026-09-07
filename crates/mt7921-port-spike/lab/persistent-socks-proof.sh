#!/usr/bin/env bash
# Exercise the userspace Netstack3 SOCKS endpoint at startup and again after
# the former 300-second run limit. Intended to run on np beside the active run.
set -euo pipefail

out=${1:?active run directory}
sustained_after=${2:-310}
proxy=${DRV_SOCKS5_PROXY:-127.0.0.1:1080}
url=${DRV_SOCKS5_PROOF_URL:-http://example.com/}
started=$(date +%s)

prove() {
  local name=$1 body tmp
  body=$out/$name.body
  tmp=$out/$name.tmp
  if curl --fail --silent --show-error --max-time 30 --socks5-hostname "$proxy" \
      "$url" -o "$body" 2>"$out/$name.curl.log"; then
    printf 'internet_proof_socks=true phase=%s realtime=%s elapsed_seconds=%s bytes=%s sha256=%s url=%s\n' \
      "$name" "$(date -u +%FT%TZ)" "$(( $(date +%s) - started ))" \
      "$(wc -c < "$body")" "$(sha256sum "$body" | cut -d' ' -f1)" "$url" > "$tmp"
    mv "$tmp" "$out/internet_proof_$name"
    cat "$out/internet_proof_$name"
    return 0
  fi
  rm -f "$tmp"
  return 1
}

for _ in $(seq 1 120); do
  prove socks_initial && break
  sleep 1
done
test -f "$out/internet_proof_socks_initial"
# The phone hotspot drops an otherwise-idle station after about one minute.
# Exercise a real proxied flow every 20s so the persistent association remains
# useful rather than merely retaining driver state.
while (( $(date +%s) - started + 20 < sustained_after )); do
  sleep 20
  prove socks_keepalive
done
sleep_for=$((sustained_after - ($(date +%s) - started)))
((sleep_for <= 0)) || sleep "$sleep_for"
prove socks_sustained
