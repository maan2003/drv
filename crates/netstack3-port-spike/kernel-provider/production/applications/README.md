# Application acceptance (in progress)

Guest-only workloads for the production no-native-INET socket frontend.
These tests are not host-network acceptance. Do not run against the host's
browser profile, daemon sockets, credentials or package store.

Stage existing executable closures (no Nix build) for curl, Git, Node and Firefox
into a disposable KVM root, alongside the provider, separate DNS service and
`libnss_drv`. Use `hosts: files drv` and `nameserver 127.0.0.1`.
Keep Firefox's sandbox enabled; use a non-root test user, writable private home
and `/dev/shm`, with `network.trr.mode=5` in the disposable profile.
The initial browser is headless/software-rendered; GPU work is not a dependency.
The standard guest must retain `CONFIG_INET=n`.

Start `http-fixture.js` with Node, then run `check-cli.sh`. The browser's fixture
page checks a redirected 1MiB response by SHA256, uploads it, and downloads eight
responses concurrently. It reports to `/run/applications/browser-result`;
a process exit or screenshot alone is not a PASS. Also load an external HTTPS
page to exercise the browser's DNS/TLS path, not just localhost.

Observed on kernel #18: curl 1MiB upload/download integrity, DNS/TLS and Git HTTPS
pass; Firefox 155.0.1 passes the JavaScript workload and visually renders the
external Example Domain HTTPS page. Screenshot: `firefox-external.png`.
Run `check-browser.sh` as the non-root app user. Its screenshot marker is not a
visual PASS until the image is inspected.

Architecture-check every existing executable: np also stores ARM64 closures.
The initially discovered Firefox 153.0.3 was ARM64, unsuitable for this guest.
The tested browser is Mozilla's official prebuilt Linux x86-64 Firefox 155.0.1,
with existing x86-64 shared libraries staged in their original paths. Preserve
`/lib` in the browser's library search path so glibc can load `libnss_drv.so.2`.
Supply fonts and fontconfig configuration even for headless rendering.

Firefox content/socket/RDD/utility processes showed seccomp filters and
no-new-privileges. Its normal forkserver did not; this is not a claim that every
browser process has the same confinement. No sandbox-disable flags were used.
The successful external screenshot run had no graphics error in its log.
 Follow-ups: WebSocket/local IPv6, HTTP2
and separate QUIC probes, disposable Git SSH transfers, FFmpeg, private VPN
recovery. Nix fetch/substitution only in isolated state/store, never Nix builds.

## Preserved compatibility failures

- Node 24.18.1 IPv6 listen: `node http-fixture.js ::1` requests
  `setsockopt(IPPROTO_IPV6, IPV6_V6ONLY, 0)` and receives ENOPROTOOPT before bind.
  Default IPv4 mode remains separately testable; IPv6 is not counted as passing.
- curl HTTP/2 succeeded. HTTP/3-only to `cloudflare-quic.com` failed with exit55;
  the same host curl reached HTTP/3. Guest traces show UDP_SEGMENT ancillary
  data on 2400-byte sends and asynchronous EMSGSIZE errors. Ancillary-message
  handling is under investigation; do not claim QUIC support from HTTP/2 success.

For WebSocket tests, install the locked `ws` fixture dependency with
`npm ci --ignore-scripts --no-audit --no-fund` in a disposable staging directory
and copy its `node_modules` beside `http-fixture.js` in the guest.
The fixture uses an upstream WebSocket implementation, not a hand-written codec.
