#!/run/current-system/sw/bin/bash
export PATH=/run/wrappers/bin:/run/current-system/sw/bin:$PATH
wdev(){ for d in /sys/class/net/wl*; do [ -e "$d/wireless" ] && { basename "$d"; return 0; }; done; return 1; }
log(){ logger -t returnnet "$*"; echo "$(date -u +%T) returnnet $*"; }
d=""
for i in $(seq 1 60); do d=$(wdev) && [ -n "$d" ] && break; sleep 1; done
[ -z "$d" ] && { log "no wifi netdev after 60s"; exit 1; }
log "dev=$d joining ajay"
for attempt in 1 2 3; do
  iwctl station "$d" disconnect >/dev/null 2>&1; sleep 2
  iwctl station "$d" connect ajay >/dev/null 2>&1
  for j in $(seq 1 20); do sleep 1; ping -c1 -W2 1.1.1.1 >/dev/null 2>&1 && { log "ajay OK internet up attempt=$attempt dev=$d"; exit 0; }; done
  log "ajay attempt=$attempt no internet; retry"
done
log "ajay failed 3x; fallback ph1 for LAN access dev=$d"
iwctl station "$d" disconnect >/dev/null 2>&1; sleep 2
iwctl station "$d" connect ph1 >/dev/null 2>&1
# wait for the ph1 association + DHCP address (SAE+DHCP can take >30s)
for k in $(seq 1 60); do
  sleep 1
  a=$(ip -4 -o addr show dev "$d" 2>/dev/null | awk '{print $4}' | cut -d/ -f1)
  case "$a" in 10.77.0.*) log "on ph1 fallback after ${k}s; reachable via redwood LAN $a"; exit 2;; esac
done
log "ph1 fallback: no LAN address after 60s dev=$d"
exit 3
