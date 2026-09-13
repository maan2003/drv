#!/bin/busybox sh
# Guest-only disposable SSH and Git fixture. No agent or production keys.
set -eu
zcat /proc/config.gz | grep -q '^# CONFIG_INET is not set$'
work=$(mktemp -d /run/ssh-application.XXXXXX)
grep -q '^sshd:' /etc/passwd || echo 'sshd:x:74:74:sshd:/var/empty:/bin/false' >>/etc/passwd
mkdir -p /var/empty
chown 0:0 /var/empty
chmod 755 /var/empty
ssh-keygen -q -t ed25519 -N '' -f "$work/client"
ssh-keygen -q -t ed25519 -N '' -f "$work/host"
cat >"$work/config" <<EOF
Port 2223
ListenAddress 127.0.0.1
HostKey $work/host
PidFile $work/sshd.pid
AuthorizedKeysFile $work/client.pub
PermitRootLogin prohibit-password
PasswordAuthentication no
KbdInteractiveAuthentication no
AuthenticationMethods publickey
UsePAM no
UseDNS no
AllowUsers root
AllowTcpForwarding no
X11Forwarding no
PermitTunnel no
EOF
/bin/sshd -D -e -f "$work/config" >"$work/server.log" 2>&1 &
server=$!
trap 'kill "$server" 2>/dev/null || true; wait "$server" 2>/dev/null || true' EXIT
printf '[127.0.0.1]:2223 %s\n' "$(cat "$work/host.pub")" >"$work/known_hosts"
export GIT_SSH_COMMAND="ssh -F /dev/null -p2223 -i $work/client -oIdentitiesOnly=yes -oBatchMode=yes -oStrictHostKeyChecking=yes -oUserKnownHostsFile=$work/known_hosts"
sleep 1
dd if=/dev/urandom of="$work/payload" bs=1048576 count=8 2>/dev/null
timeout 30 sh -c "$GIT_SSH_COMMAND root@127.0.0.1 cat" <"$work/payload" >"$work/roundtrip"
cmp "$work/payload" "$work/roundtrip"
echo PASS_SSH_8MIB_ROUNDTRIP
git init -q "$work/source"
cp "$work/payload" "$work/source/payload"
git -C "$work/source" add payload
git -C "$work/source" -c user.name=fixture -c user.email=fixture@invalid commit -qm fixture
timeout 30 git clone -q --upload-pack="/bin/git upload-pack" "ssh://root@127.0.0.1$work/source" "$work/clone"
cmp "$work/payload" "$work/clone/payload"
echo PASS_GIT_SSH_CLONE
