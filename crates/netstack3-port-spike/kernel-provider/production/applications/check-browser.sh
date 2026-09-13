#!/bin/busybox sh
# Guest-only Firefox check. FIREFOX points to a verified x86-64 executable.
set -eu
zcat /proc/config.gz | grep -q '^# CONFIG_INET is not set$'
: "${FIREFOX:=/opt/firefox/firefox}"
. /etc/applications/firefox-env
export HOME=/home/app FONTCONFIG_FILE=/etc/fonts/fonts.conf
export MOZ_HEADLESS=1 LIBGL_ALWAYS_SOFTWARE=1
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY all_proxy
mkdir -p "$HOME/profile" "$HOME/external"
cp /etc/applications/firefox-user.js "$HOME/profile/user.js"
cp /etc/applications/firefox-user.js "$HOME/external/user.js"
rm -f /run/applications/browser-result
"$FIREFOX" --headless --no-remote --profile "$HOME/profile" \
 http://127.0.0.1:8080/ >/run/applications/firefox-local.log 2>&1 &
browser=$!
trap 'kill "$browser" 2>/dev/null || true; wait "$browser" 2>/dev/null || true' EXIT
for i in $(seq 1 120); do
 test ! -s /run/applications/browser-result || break
 kill -0 "$browser"
 sleep .5
done
test "$(cat /run/applications/browser-result)" = PASS_BROWSER_HTTP_WEBSOCKET
echo PASS_FIREFOX_JAVASCRIPT
kill "$browser"
wait "$browser" || true
trap - EXIT
rm -f /run/applications/firefox-external.png
timeout 90 "$FIREFOX" --headless --no-remote --profile "$HOME/external" \
 --screenshot /run/applications/firefox-external.png https://example.com/ \
 >/run/applications/firefox-external.log 2>&1
test -s /run/applications/firefox-external.png
echo FIREFOX_EXTERNAL_SCREENSHOT_READY_FOR_VISUAL_REVIEW
