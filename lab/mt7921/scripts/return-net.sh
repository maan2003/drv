#!/run/current-system/sw/bin/bash
export PATH=/run/wrappers/bin:/run/current-system/sw/bin:$PATH
wdev(){ for d in /sys/class/net/wl*; do [ -e "$d/wireless" ] && { basename "$d"; return 0; }; done; return 1; }
log(){ logger -t returnnet "$*"; echo "$(date -u +%T) returnnet $*"; }
# iwd only connects to networks in its own scan results; scan and wait until the
# SSID appears in get-networks before connecting (cfg80211 expires entries ~30s).
join(){ local d=$1 ssid=$2 i
  iwctl station "$d" disconnect >/dev/null 2>&1; sleep 2
  iwctl station "$d" scan >/dev/null 2>&1
  for i in $(seq 1 12); do sleep 1; iwctl station "$d" get-networks 2>/dev/null \
    | sed 's/\x1b\[[0-9;]*m//g' | grep -qE "[[:space:]]${ssid}[[:space:]]" && break; done
  iwctl station "$d" connect "$ssid" >/dev/null 2>&1
}
d=""
for i in $(seq 1 60); do d=$(wdev) && [ -n "$d" ] && break; sleep 1; done
[ -z "$d" ] && { log "no wifi netdev after 60s"; exit 1; }
log "dev=$d joining ajay"
for attempt in 1 2 3; do
  join "$d" ajay
  for j in $(seq 1 20); do sleep 1; ping -c1 -W2 1.1.1.1 >/dev/null 2>&1 && { log "ajay OK internet up attempt=$attempt dev=$d"; exit 0; }; done
  log "ajay attempt=$attempt no internet; retry"
done
log "ajay failed 3x; watchdog-owned reboot remains the recovery path dev=$d"
exit 1
