#!/run/current-system/sw/bin/bash
export PATH=/run/wrappers/bin:/run/current-system/sw/bin:$PATH
D=/sys/kernel/debug/ieee80211/phy0/mt76
rd(){ sudo -n sh -c "echo $1 > $D/regidx; cat $D/regval" 2>/dev/null; }
echo "## native-on-$(iwctl station wlan0 show 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' | grep -a 'Connected network' | awk '{print $3}') $(date -u +%T) mac=$(cat /sys/class/net/wlan0/address) bssid=$(iw dev wlan0 link 2>/dev/null | awk '/Connected to/{print $3}')"
echo "RFCR=$(rd 0x820e5000) RFCR1=$(rd 0x820e5004)"
echo "RMAC block 0x820e5000..0x820e57fc (16 words per line):"
for base in $(seq 0 64 2044); do
  printf "  0x%08x:" $((0x820e5000+base))
  for o in $(seq 0 4 60); do a=$(printf "0x%08x" $((0x820e5000+base+o))); v=$(rd $a); printf " %s" "${v#0x}"; done; echo
done
for w in 0 1 2 3; do b=$((0x820d8000 + w*0x100)); printf "WTBL wcid%d dw0-9:" $w; for i in $(seq 0 9); do a=$(printf "0x%08x" $((b+i*4))); printf " %s" "$(rd $a)"; done; echo; done
echo "DONE $(date -u +%T)"
