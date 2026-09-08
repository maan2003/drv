#!/run/current-system/sw/bin/bash
export PATH=/run/wrappers/bin:/run/current-system/sw/bin:/home/user/.nix-profile/bin:/nix/store/aka3355fd0b3v2yvzscfllzwbb9x7iz6-iw-6.17/bin:$PATH
set -uo pipefail
bdf=$1
lab=/run/current-system/sw/bin/wifi-driver-lab
wd=/run/current-system/sw/bin/wifi-lab-watchdog
launcher=/data/persist/drvlab/active-launcher.sh
ret=/data/persist/drvlab/return-net.sh
# Target: ajay (the phone hotspot). It does its own NAT to the internet, so the
# whole run only needs this host + the phone -- no redwood AP, NAT, firewall, or
# hostapd deauth guard.
ap_ssid=ajay
ap_bssid=02:d3:b9:dd:c3:d0
out=/data/persist/drvlab/active-run-$(date -u +%Y%m%dT%H%M%SZ); mkdir -p "$out"; cd "$out"
exec > run.log 2>&1
echo "start $(date -u +%FT%TZ) bdf=$bdf target=$ap_ssid/$ap_bssid"
wdev(){ for d in /sys/class/net/wl*; do [ -e "$d/wireless" ] && { basename "$d"; return 0; }; done; return 1; }
# safety net: userspace recovery fires at 480s regardless (dynamic dev, ajay->ph1 fallback)
systemd-run --on-active=480 --unit=drvlab-active-return-$$ "$ret" >/dev/null 2>&1
journalctl -f -o short-precise --no-tail _TRANSPORT=kernel + _SYSTEMD_UNIT=iwd.service > kernel-iwd-journal.log 2>&1 & jpid=$!
sudo -n iw event -t -f > iw-event.log 2>&1 & iwpid=$!
connect_ap() {
  local d a; d=$(wdev) || return 1
  iwctl station "$d" disconnect >/dev/null 2>&1; sleep 1
  for a in 1 2 3 4; do
    # iwd only connects to networks in its OWN scan results ("Invalid network
    # name" otherwise), and cfg80211 expires scan entries after 30 s, so scan
    # and wait until ajay actually shows up in get-networks before connecting.
    iwctl station "$d" scan >/dev/null 2>&1
    for i in $(seq 1 12); do sleep 1; iwctl station "$d" get-networks 2>/dev/null \
      | sed 's/\x1b\[[0-9;]*m//g' | grep -qE "[[:space:]]${ap_ssid}[[:space:]]" && break; done
    echo "scan $ap_ssid dev=$d attempt=$a $(date -u +%T.%N)"
    if iwctl station "$d" connect "$ap_ssid"; then
      for i in $(seq 1 40); do sleep 1; iw dev "$d" link 2>/dev/null | grep -qi "Connected to $ap_bssid" && return 0; done
    fi
    sleep 3
  done
  return 1
}
if ! connect_ap; then echo retry; connect_ap || { echo "native precondition failed"; "$ret" & kill $jpid; sudo -n kill $iwpid 2>/dev/null; exit 1; }; fi
echo "native $ap_ssid link ok $(date -u +%T.%N)"
sudo -n $wd status | grep -q disarmed || sudo -n $wd fire >/dev/null 2>&1 || true
token=$(sudo -n $wd arm) || { echo "watchdog arm failed"; "$ret" & kill $jpid; sudo -n kill $iwpid 2>/dev/null; exit 1; }
echo "watchdog armed"
# heartbeat runs THROUGH the run AND recovery, so a wedged VFIO restore still reboots
# The marker must exist BEFORE the loop starts (run 110031Z: the subshell won the
# race, saw no marker, exited at once, and the 120 s lease rebooted np mid-recovery).
# A single failed heartbeat is retried, not fatal.
touch "$out/.hb"
( while [ -f "$out/.hb" ]; do sudo -n $wd heartbeat "$token" >/dev/null 2>&1 || echo "heartbeat failed $(date -u +%T)"; sleep 20; done ) & hb=$!
sudo -n $wd heartbeat "$token" >/dev/null 2>&1 || echo "initial heartbeat failed"
# leave the AP cleanly before the VFIO unbind hands the radio to the driver.
dn=$(wdev); iwctl station "$dn" disconnect >/dev/null 2>&1
sleep 1; echo "native disconnected $(date -u +%T.%N)"
sleep 2
echo "run lab $(date -u +%T.%N)"
sudo -n $lab $bdf 300 -- $launcher > lab.out 2>&1; echo "lab rc=$? $(date -u +%T.%N)"
sleep 3
R=$(sudo -n bash -c "ls -t /var/lib/wifi-driver-lab/reports/*.log 2>/dev/null | head -1")
echo "report=$R"; sudo -n cp "$R" "$out/report.log" 2>/dev/null; sudo -n chmod a+r "$out/report.log" 2>/dev/null
# recover network with watchdog STILL armed; only disarm once the network is confirmed
echo "return net $(date -u +%T.%N)"; "$ret"; rc=$?; echo "return rc=$rc"
# confirm reachable state before disarming; if not reachable, leave watchdog to reboot.
# retry up to ~120s (the heartbeat keeps feeding the watchdog during this loop).
# Checks: internet ping (ajay NATs), OR ph1 LAN fallback address, OR iwctl connected.
confirmed=0
for i in $(seq 1 24); do
  d=$(wdev || echo wlan0)
  if ping -c1 -W2 1.1.1.1 >/dev/null 2>&1 \
     || ip -4 -o addr show dev "$d" 2>/dev/null | grep -qE " (172\.20\.10|10\.77\.0)\." \
     || iwctl station "$d" show 2>/dev/null | sed "s/\x1b\[[0-9;]*m//g" | grep -qiE "^ *State +connected"; then
    echo "network confirmed after $((i*5))s (dev=$d)"; confirmed=1; break; fi
  sleep 5
done
if [ "$confirmed" = 1 ]; then
  echo "network confirmed; disarming watchdog"
  rm -f "$out/.hb"; kill $hb 2>/dev/null
  sudo -n $wd disarm "$token" >/dev/null 2>&1 || true
else
  echo "network NOT confirmed; leaving watchdog armed to reboot for recovery"
  rm -f "$out/.hb"; kill $hb 2>/dev/null
  # do NOT disarm: let the 120s lease expire -> reboot -> TPM unlock -> iwd autoconnect
fi
kill $jpid 2>/dev/null; sudo -n kill $iwpid 2>/dev/null
echo "end $(date -u +%FT%TZ)"
