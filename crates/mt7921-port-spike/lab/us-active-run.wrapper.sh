#!/run/current-system/sw/bin/bash
export PATH=/run/wrappers/bin:/run/current-system/sw/bin:/home/user/.nix-profile/bin:/nix/store/aka3355fd0b3v2yvzscfllzwbb9x7iz6-iw-6.17/bin:$PATH
set -uo pipefail
bdf=$1
lab=/run/current-system/sw/bin/wifi-driver-lab
wd=/run/current-system/sw/bin/wifi-lab-watchdog
launcher=/data/persist/drvlab/active-launcher.sh
ret=/data/persist/drvlab/return-net.sh
out=/data/persist/drvlab/active-run-$(date -u +%Y%m%dT%H%M%SZ); mkdir -p "$out"; cd "$out"
exec > run.log 2>&1
echo "start $(date -u +%FT%TZ) bdf=$bdf"
wdev(){ for d in /sys/class/net/wl*; do [ -e "$d/wireless" ] && { basename "$d"; return 0; }; done; return 1; }
# safety net: userspace recovery fires at 480s regardless (dynamic dev, ajay->ph1 fallback)
systemd-run --on-active=480 --unit=drvlab-active-return-$$ "$ret" >/dev/null 2>&1
journalctl -f -o short-precise --no-tail _TRANSPORT=kernel + _SYSTEMD_UNIT=iwd.service > kernel-iwd-journal.log 2>&1 & jpid=$!
sudo -n iw event -t -f > iw-event.log 2>&1 & iwpid=$!
connect_ph1() {
  local d a; d=$(wdev) || return 1
  iwctl station "$d" disconnect >/dev/null 2>&1; sleep 1
  for a in 1 2 3 4; do
    # Refresh the kernel BSS cache right before connecting: cfg80211 expires
    # scan entries after 30 s and CMD_AUTHENTICATE then fails immediately
    # (iwd "connect-failed, status: 1" with no auth frame on air; seen when np
    # had been sitting on ajay for a while: runs 1788757698/1788757875/1788763175).
    # iwd only trusts its own scan results ("Invalid network name 'ph1'" with
    # a raw iw scan seeing the BSS, run 1788777538), so ask iwd to scan and
    # wait until ph1 shows up in get-networks.
    iwctl station "$d" scan >/dev/null 2>&1
    for i in $(seq 1 10); do sleep 1; iwctl station "$d" get-networks 2>/dev/null | grep -q " ph1 " && break; done
    echo "scan ph1 dev=$d attempt=$a iwd_sees=$(iwctl station "$d" get-networks 2>/dev/null | grep -c " ph1 ") $(date -u +%T.%N)"
    echo "connect ph1 dev=$d attempt=$a $(date -u +%T.%N)"
    if iwctl station "$d" connect ph1; then
      for i in $(seq 1 40); do sleep 1; iw dev "$d" link 2>/dev/null | grep -q "Connected to 72:a6:c7:7d:56:93" && return 0; done
    fi
    sleep 3
  done
  return 1
}
if ! connect_ph1; then echo retry; connect_ph1 || { echo "native precondition failed"; "$ret" & kill $jpid; sudo -n kill $iwpid 2>/dev/null; exit 1; }; fi
echo "native ph1 link ok $(date -u +%T.%N)"
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
# leave ph1 cleanly: the VFIO unbind sends no deauth, and a stale [MFP] entry
# makes hostapd answer the driver's association with an SA Query comeback.
dn=$(wdev); iwctl station "$dn" disconnect >/dev/null 2>&1
# Observed run 054446Z: this disconnect produced no deauth at the AP, so the
# driver still met a status-30 SA Query comeback (~1 s) and then associated.
# The launch helper (lab/launch-active-run.sh) deauths the client on the AP
# beforehand so the native precondition itself is not hit by the comeback.
sleep 1; echo "native disconnected $(date -u +%T.%N)"
# give the launch helper's AP guard (deauth on redwood once np stops answering
# ping) time to clear the stale entry before the driver's SAE starts.
sleep 2
echo "run lab $(date -u +%T.%N)"
sudo -n $lab $bdf 300 -- $launcher > lab.out 2>&1; echo "lab rc=$? $(date -u +%T.%N)"
sleep 3
R=$(sudo -n bash -c "ls -t /var/lib/wifi-driver-lab/reports/*.log 2>/dev/null | head -1")
echo "report=$R"; sudo -n cp "$R" "$out/report.log" 2>/dev/null; sudo -n chmod a+r "$out/report.log" 2>/dev/null
# recover network with watchdog STILL armed; only disarm once internet OR ph1 LAN is confirmed
echo "return net $(date -u +%T.%N)"; "$ret"; rc=$?; echo "return rc=$rc"
# confirm reachable state before disarming; if not reachable, leave watchdog to reboot
# retry up to ~120s (the heartbeat keeps feeding the watchdog during this loop, so the
# lease cannot expire here). ph1 fallback SAE+DHCP can take >30s after return-net returns.
# Checks: internet ping, OR redwood LAN gw 10.77.0.1, OR a ph1 LAN address on the wifi dev,
# OR iwctl reporting the station connected. No iw dependency.
confirmed=0
for i in $(seq 1 24); do
  d=$(wdev || echo wlan0)
  if ping -c1 -W2 1.1.1.1 >/dev/null 2>&1 \
     || ping -c1 -W2 10.77.0.1 >/dev/null 2>&1 \
     || ip -4 -o addr show dev "$d" 2>/dev/null | grep -q " 10\.77\.0\." \
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
