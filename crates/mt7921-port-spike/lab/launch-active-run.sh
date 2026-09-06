#!/usr/bin/env bash
# Launch one active-client run on np with the given store path, and keep the
# redwood AP free of a stale association for the session client MAC.
#
# Background (2026-09-07): the wrapper's native iwd precondition leaves ph1
# with `iwctl station disconnect`; the kernel logs "deauthenticating ... by
# local choice" but the AP usually does not receive that deauth, so hostapd
# keeps an [AUTHORIZED][MFP] entry. The driver's association then gets
# status 30 (SA Query comeback); after the ~1 s comeback hostapd accepts the
# retry but spends ~1.2 s in DEL_STATION for the stale entry before answering,
# and the response arrives after the driver stopped listening. A leftover
# entry also makes the *native* precondition itself fail ("Operation failed").
#
# So: (1) pre-deauth before launch (np autoconnects back if it was on ph1),
# and (2) an "AP guard" on redwood: every time a *native* session (fresh,
# authorized, answering ping) stops answering ping, deauth the client at once.
# That fires ~1.5 s after np leaves ph1, and the wrapper waits 3 s before the
# VFIO handoff, so the driver's own SAE starts on a clean AP. The driver's
# session never answers ping (no IP yet), so the guard never touches it.
set -u
store="${1:?store path}"
ssh_cfg="${SSH_CFG:-$HOME/.ssh/lab.conf}"
client="${DRV_SAE_CLIENT_MAC:-8a:fd:2a:8b:70:5a}"
hcli=/nix/store/hprndjc6hwr2nd8sxyjj3kwzmqbfwpq5-hostapd-2.11/bin/hostapd_cli
here=$(dirname "$0")
e=$(date -u +%s)
# redwood's NixOS firewall drops new input by default; dnsmasq on ap0 never saw
# a DHCP Discover until 2026-09-07. Re-add the runtime allow rule if missing.
ssh -F "$ssh_cfg" redwood 'sudo -n nft list chain inet nixos-fw input-allow 2>/dev/null | grep -q drvlab_dhcp_dns \
  || { sudo -n nft add rule inet nixos-fw input-allow iifname "ap0" meta l4proto { udp, tcp } th dport { 53, 67 } accept comment drvlab_dhcp_dns && echo "ap0 dhcp/dns firewall rule added"; }
  sudo -n nft list table ip drvlab >/dev/null 2>&1 || { sudo -n nft -f /var/lib/drvlab/nat.nft && echo "drvlab NAT table reloaded"; }'
ssh -F "$ssh_cfg" redwood "sudo -n $hcli -p /run/hostapd -i ap0 deauthenticate $client >/dev/null 2>&1; \
  for i in 1 2 3 4 5; do n=\$(sudo -n $hcli -p /run/hostapd -i ap0 all_sta | grep -cE '^[0-9a-f:]{17}\$'); [ \"\$n\" = 0 ] && break; sleep 1; done; echo ap-stations=\$n
nohup bash -c '
  c=$client; h=$hcli; t0=$e; deauths=0
  while [ \$(( \$(date +%s) - t0 )) -lt 170 ]; do
    ip=\"\"
    # phase 1: a fresh native session (associated after t0, authorized, pings)
    while [ \$(( \$(date +%s) - t0 )) -lt 170 ]; do
      info=\$(sudo -n \$h -p /run/hostapd -i ap0 sta \$c 2>/dev/null)
      age=\$(echo \"\$info\" | sed -n \"s/^connected_time=//p\")
      ip=\$(ip -4 neigh show dev ap0 | awk -v m=\$c \"\\\$0 ~ m {print \\\$1; exit}\")
      if [ -n \"\$age\" ] && [ \$age -le \$(( \$(date +%s) - t0 + 2 )) ] && echo \"\$info\" | grep -q AUTHORIZED \
         && [ -n \"\$ip\" ] && ping -c1 -W1 \$ip >/dev/null 2>&1; then break; fi
      ip=\"\"; sleep 0.5
    done
    [ -z \"\$ip\" ] && break
    echo \"ap-guard: native session up ip=\$ip age=\$age \$(date -u +%T.%N)\"
    # phase 2: the instant it stops answering, clear the AP entry
    fails=0
    while [ \$(( \$(date +%s) - t0 )) -lt 170 ]; do
      if ping -c1 -W1 \$ip >/dev/null 2>&1; then fails=0; else fails=\$((fails+1)); fi
      [ \$fails -ge 2 ] && break
      sleep 0.2
    done
    sudo -n \$h -p /run/hostapd -i ap0 deauthenticate \$c >/dev/null 2>&1; deauths=\$((deauths+1))
    echo \"ap-guard: deauth #\$deauths sent \$(date -u +%T.%N) stations=\$(sudo -n \$h -p /run/hostapd -i ap0 all_sta | grep -cE \"^[0-9a-f:]{17}\$\")\"
    sleep 1
  done
  echo \"ap-guard: done deauths=\$deauths \$(date -u +%T)\"
' > /tmp/ap-guard-$e.log 2>&1 < /dev/null & echo ap-guard-started log=/tmp/ap-guard-$e.log"
# np may have been kicked off ph1 by the pre-deauth; iwd autoconnects back.
for i in $(seq 1 20); do "$here/np-ssh.sh" -t 6 true 2>/dev/null && break; sleep 4; done
"$here/np-ssh.sh" "(nohup setsid /data/persist/drvlab/regen-and-run.sh $store > /data/persist/drvlab/regen-run-$e.log 2>&1 < /dev/null &); sleep 2; cat /data/persist/drvlab/regen-run-$e.log"
echo "launch-epoch=$e log=/data/persist/drvlab/regen-run-$e.log ap-guard-log=redwood:/tmp/ap-guard-$e.log"
