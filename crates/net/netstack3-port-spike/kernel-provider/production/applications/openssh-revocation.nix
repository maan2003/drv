{ lib, openssh }:

assert lib.assertMsg (openssh.version == "10.4p1")
  "openssh-revocation.patch is pinned to OpenSSH 10.4p1";
openssh.overrideAttrs (old: {
  pname = "openssh-drv";
  patches = (old.patches or [ ]) ++ [ ./openssh-listener-revocation.patch ];
})
