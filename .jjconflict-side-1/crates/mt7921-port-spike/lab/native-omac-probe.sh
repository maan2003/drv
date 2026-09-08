#!/run/current-system/sw/bin/bash
export PATH=/run/wrappers/bin:/run/current-system/sw/bin:$PATH
D=/sys/kernel/debug/ieee80211/phy0/mt76
rd(){ sudo -n sh -c "echo $1 > $D/regidx; cat $D/regval" 2>/dev/null; }
st(){ iwctl station wlan0 show 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' | grep -a -E '^ *State' | awk '{print $2}'; }
dumpT(){ echo "$(date -u +%T) $1: e0=$(rd 0x820e5200)/$(rd 0x820e5204) e1=$(rd 0x820e5208)/$(rd 0x820e520c) x210=$(rd 0x820e5210) bssid38=$(rd 0x820e5038)/$(rd 0x820e503c) x180=$(rd 0x820e5180)/$(rd 0x820e5184) rfcr=$(rd 0x820e5000) mac=$(cat /sys/class/net/wlan0/address) up=$(cat /sys/class/net/wlan0/operstate) state=$(st)"; }
dumpT connected
iwctl station wlan0 disconnect >/dev/null 2>&1; sleep 2; dumpT disconnected
sudo -n ip link set wlan0 down; sleep 2; dumpT down
sudo -n ip link set wlan0 up; sleep 3; dumpT up
sleep 5; dumpT up+5s
iwctl station wlan0 connect ajay >/dev/null 2>&1
for k in $(seq 1 40); do sleep 1; [ "$(st)" = connected ] && { echo "connected after ${k}s"; break; }; done
dumpT reconnected
sleep 5; dumpT reconnected+5s
for k in $(seq 1 30); do ping -c1 -W2 1.1.1.1 >/dev/null 2>&1 && { echo "internet ok after ${k}"; break; }; sleep 1; done
echo "DONE $(date -u +%T)"
