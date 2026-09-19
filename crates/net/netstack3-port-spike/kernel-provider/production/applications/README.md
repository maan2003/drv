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

## Compatibility findings

- Node 24.18.1 IPv6 listen: `node http-fixture.js ::1` requests
  `setsockopt(IPPROTO_IPV6, IPV6_V6ONLY, 0)` and receives ENOPROTOOPT before bind.
  Default IPv4 mode remains separately testable; IPv6 is not counted as passing.
- curl HTTP/2 succeeded. HTTP/3-only initially failed with exit55 on kernel #18.
  The trace exposed silently ignored UDP_SEGMENT ancillary data. Kernel #19
  rejects that request with EIO before admission; curl resends two ordinary
  1200-byte datagrams and completes real HTTP/3. The regression and fallback trace
  are recorded in `../rust-abi5/ancillary-evidence.txt`. GSO itself is not implemented.
- `check-ssh.sh` passes an 8MiB byte-exact SSH roundtrip and Git clone through
  localhost, using fresh fixture-only keys and pinned host-key verification.
  Unsupported IP_TOS/VRF queries produce warnings but do not prevent this gate.

### OpenSSH listener revocation

The production image must stage `packages.<system>.openssh-revocation` instead
of the stock OpenSSH package. The package pins the adaptation to OpenSSH 10.4p1
and applies `openssh-listener-revocation.patch` at the application packaging
boundary. When poll reports `POLLHUP` or `POLLNVAL`, sshd closes only that
terminal listener. It retains healthy listeners and continues to treat
`POLLERR` and an `ENETDOWN` from accept as potentially transient. If no listener
remains, sshd exits instead of repeatedly logging accept failures.

Build the deployable closure and deterministic application regression with:

```sh
nix build .#openssh-revocation -o result-openssh-revocation
nix build .#checks.x86_64-linux.openssh-listener-revocation
```

The regression injects the production frontend's terminal poll contract into
the actual patched sshd, verifies a second listener completes an SSH transport
handshake, and verifies a sole terminal listener logs once and exits. A separate
case injects `POLLERR` plus one transient `ENETDOWN` from accept and then
completes a handshake on the retained listener.

For WebSocket tests, install the locked `ws` fixture dependency with
`npm ci --ignore-scripts --no-audit --no-fund` in a disposable staging directory
and copy its `node_modules` beside `http-fixture.js` in the guest.
The fixture uses an upstream WebSocket implementation, not a hand-written codec.

Tested Mozilla Linux x86-64 archive SHA256:
`642ab731354a5ca790b894d4556dfb5028c61d0c24eb10d10e10a111a69c89bf`.
No Nix builds were used. Existing FFmpeg store artifacts also proved ARM64;
streaming/VPN recovery and isolated Nix fetches remain follow-up gates.

## Service-generation recovery gate

`recovery-guest-init` runs `recovery.js` in the same disposable no-INET image,
with the provider started directly (no Ethernet FD). This is **100 loopback
provider-generation/DNS-restart cycles**, not 100 Wi-Fi, DHCP or WAN reconnections.
The production Quad9 configuration and public-network evidence remain separate.
The local HTTP/2 TLS A/AAAA upstream is fixture-only; no plaintext DNS fallback
is added to the service.

Stage the current `../loopback-test.c` binary and `recovery.{js,toml}` alongside
the existing application files and `ws` dependency. Generate a disposable CA
and server certificate on the build host; do not copy real private keys:

```sh
umask 077
openssl req -x509 -newkey rsa:2048 -nodes -keyout ca-key.pem \
  -out recovery-ca.pem -days 2 -subj '/CN=drv disposable recovery CA' \
  -addext 'basicConstraints=critical,CA:TRUE'
openssl req -newkey rsa:2048 -nodes -keyout recovery-key.pem \
  -out server.csr -subj '/CN=recovery.test'
printf '%s\n' 'subjectAltName=DNS:recovery.test' \
  'basicConstraints=critical,CA:FALSE' \
  'keyUsage=digitalSignature,keyEncipherment' \
  'extendedKeyUsage=serverAuth' > extensions
openssl x509 -req -in server.csr -CA recovery-ca.pem -CAkey ca-key.pem \
  -CAcreateserial -out recovery-cert.pem -days 2 -extfile extensions
```

Copy only `recovery-{ca,cert,key}.pem` into guest `/etc/applications/`;
CA/certificate mode 0644, fixture key 0640 (init sets its group to app).
Keep the CA private key outside the guest. Run the existing private KVM runner
with `QEMU_MEMORY_MIB=4096` and this optional init:

```sh
QEMU_MEMORY_MIB=4096 ../run-tailscale-kvm.sh KERNEL ROOT NEW_OUTPUT \
  recovery-guest-init
```

The log is retained in `/run/recovery.log` and the host runner’s `serial.log`. Success requires
`PASS RECOVERY_100_LOOPBACK_GENERATIONS`, not merely a live VM. The runner's
one-hour bound still applies. Capture the log before stopping the private VM.

Each cycle:

- Restarts DNS alone while Firefox, its same identified WebSocket, the HTTP
  server and retained IPv4/IPv6 TCP/UDP descriptors remain alive. Browser
  heartbeat receipts carry unique phase nonces and verified 128KiB download/
  upload hashes; sequence and connection identity reject stale results.
- Uses unique wire-DNS names over both loopback families and glibc NSS names;
  the TLS fixture must observe uncached A/AAAA requests.
- Kills only Netstack3, then waits for the observer's blocked TCP/UDP reads and
  blocked TCP writes to fail with ENETDOWN. Before death, writers first reach
  nonblocking EAGAIN backpressure, and `/proc/PID/{status,syscall}` must show
  all six workers sleeping inside the intended syscall on the expected FD.
  Retained listeners expose terminal
  readiness, including after SO_ERROR consumption; accept fails with ENETDOWN.
  New IPv4/IPv6 sockets fail closed while the provider is absent.
- Observes DNS exiting on dead listener errors before reaping it and removing
  its NSS pathname. Only after the old-socket barrier does the orchestrator
  replace the application server, provider and DNS. Firefox stays running,
  observes a WS close, reconnects and exchanges new-generation traffic.
- Exercises fresh IPv4/IPv6 TCP byte integrity and UDP batches, then verifies
  the original descriptors remain dead. Listener replacement is explicit
  orchestration, not transparent restoration of lost transport state.

Deadlines: provider/local readiness 10s; blocked worker safety alarm 15s
(observer barrier 10s); DNS death and child reap 5s; browser recovery 15s.
DNS bind teardown retries are limited to EADDRINUSE and 10s; other startup
failures are fatal. DNS receives real write-only pipes: Node's default stdio
“pipes” are socketpairs and correctly fail the service's descriptor contract.

Quiescent-cycle resource bounds are declared in the gate: ≤40 userspace
processes, ≤2048 FDs, RSS <2,000,000KiB and slab <512,000KiB; provider/DNS each
≤32 FDs. After cycle 10, allow at most +2 processes, +32 FDs, +256MiB RSS and
+64MiB slab over that warm baseline. Reject zombies and kernel WARN/OOPS/BUG.
A continuously drained `/dev/kmsg` collector runs from before the first
provider through final cleanup; a collector exit also fails the gate. A unique
post-cleanup `/dev/kmsg` marker must be observed before PASS. Kernel warning
severity or worse is rejected in addition to WARN/OOPS/BUG signatures.
Service pipes are continuously drained. Resource tolerances allow allocator
warmup, not indefinite accumulation.

Final kernel #21 result: **100/100 hardened cycles passed**; see
[`recovery-evidence.txt`](recovery-evidence.txt) for measurements, failure
history and the separate post-gate public Quad9/HTTP2/HTTP3/SSH regressions.
