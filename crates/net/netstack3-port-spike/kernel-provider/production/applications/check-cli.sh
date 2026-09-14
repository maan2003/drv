#!/bin/busybox sh
# Guest only, with Node fixture running and our DNS service owning loopback53.
set -eu
test "$(cat /proc/sys/kernel/hostname)" != no-plastic
zcat /proc/config.gz | grep -q '^# CONFIG_INET is not set$'
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY all_proxy
export HOME=/run/applications/home
mkdir -p "$HOME"
curl --fail --silent --show-error --max-time 30 --location \
 http://127.0.0.1:8080/redirect -o /run/applications/payload
test "$(wc -c </run/applications/payload)" = 1048576
expected=$(sha256sum /run/applications/payload | cut -d' ' -f1)
actual=$(curl --fail --silent --show-error --max-time 30 \
 --data-binary @/run/applications/payload http://127.0.0.1:8080/upload)
test "$actual" = "$expected"
echo PASS_CURL_LOOPBACK_DOWNLOAD_UPLOAD
curl --fail --silent --show-error --max-time 30 https://example.com/ \
 -o /run/applications/example.html
grep -q 'Example Domain' /run/applications/example.html
echo PASS_CURL_DNS_TLS
# Public read-only Git endpoint, isolated configuration and no credentials.
GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0 timeout 60 git \
 -c credential.helper= ls-remote https://github.com/octocat/Hello-World.git HEAD \
 >/run/applications/git-head
grep -Eq '^[0-9a-f]{40}[[:space:]]+HEAD$' /run/applications/git-head
echo PASS_GIT_HTTPS
