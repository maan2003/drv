# Sapphire WebAssembly host boundary

All Sapphire C++ is compiled into one WebAssembly instance embedded by a
dedicated sandboxed Wasmtime worker process. A separate native Rust supervisor
owns HCI and external service capabilities. No Sapphire object is linked into
or loaded as native code, and there is no native-C++ fallback.

[`wit/sapphire.wit`](wit/sapphire.wit) defines the copied capability boundary.
It imports only WASI Preview 2 monotonic-clock, poll, and secure-random
interfaces; it does not import the WASI command world. The worker validates
list sizes and queue quotas, copies bytes into and out of WASM memory, and
forwards project imports to the supervisor over bounded framed IPC. The worker
receives no raw HCI or other device, filesystem, or network descriptors.
Persistence identities and all other handles are values selected or validated
by the host rather than native pointers.

The final component import allowlist is checked exactly. Filesystem, sockets,
environment, arguments, process/exec, terminal, wall-clock, and device
interfaces must remain absent. Logging, metrics, and keyed persistence remain
project capabilities.

The supervisor implements the first `controller.send` lowering over the same
framed process socket. It validates command, ACL, SCO, and ISO length fields in
the native broker crate and enforces packet-count and byte quotas. The current
implementation terminates at a deterministic fake controller and owns no HCI
descriptor; physical transport attachment remains a separate guarded step.
Every worker generation has a fresh session identity. Calls and replies carry
monotonic correlation IDs, while event, ACL, SCO, and ISO deliveries share one
bounded FIFO and sequence space. The worker queues deliveries received during
a host import and invokes the guest callback only after the current WASM entry
returns, so Wasmtime is never re-entered from an import. Stop-and-wait delivery,
explicit completion, and response priority provide backpressure without packet
reordering or nested `Store` access.

An abnormal worker result is killed/reaped and restarted once with a new
socket, session, controller, queues, and correlation state. Old deliveries are
never replayed into the replacement session. The GAP test runtime performs one
fake Reset command/Command Complete exchange through this real process boundary
before entering the otherwise unchanged upstream suite.

The first executable target is the pinned upstream Sapphire fake-controller
GAP discovery suite. Its WASM import list must match the interfaces in the WIT
world before it can be connected to the physical HCI broker.

The separate `//:physical_discovery` reactor is the guarded physical slice. It
uses Sapphire's `Transport`, `LegacyLowEnergyScanner`, `PeerCache`, and
`LowEnergyDiscoveryManager`, and runs an active upstream discovery session.
It accepts only copied HCI packets without H4 bytes and reports only a
guest-derived unique-peer count. The manager's random dependency is a bounded
project import; the supervisor fulfills at most 4096 bytes per request with
the kernel secure-random source. A READY/START handshake validates and
instantiates the unprivileged worker before the supervisor mutates controller
state. The supervisor owns the sole Linux HCI user-channel descriptor,
allowlists only Reset, event-mask, and LE scan commands, imposes the wall
deadline, stops the session, waits for Sapphire's scan-disable completion,
restores the exact controller flags, and independently reacquires the
exclusive user channel. Physical mode has no fake fallback or automatic
worker retry.

The guarded `no-plastic` run completed a six-second LE scan through `hci0`.
Sapphire reached scanning, produced a guest-derived count of one unique peer,
issued scan disable, and reached stopped state. No peer identity is committed.
The controller began and ended down with flags `0x00000000`; both the
supervisor's post-restore probe and a separate post-run process reacquired the
exclusive user channel. The durable report was root-owned mode 0600, while
`mt7921e`, iwd, SSH, the disarmed watchdog, and the lab lock remained healthy.
This proves discovery only, not pairing, bonding, profiles, audio, or firmware
control.

Build and exercise the CPU-only exact-HCI fixture with:

```sh
SAPPHIRE_BAZEL_TARGET=//:physical_discovery \
  nix develop --command scripts/build-sapphire-gap-wasm
cargo run -p bluetooth-sapphire-wasm -- --physical-fixture \
  target/sapphire-gap-wasm/pigweed/bazel-bin/physical_discovery
for fault in worker-crash timeout malformed-ipc partial-hci; do
  cargo run -p bluetooth-sapphire-wasm -- --physical-fault-fixture "$fault" \
    target/sapphire-gap-wasm/pigweed/bazel-bin/physical_discovery
done
```

The fault fixtures never open HCI. They cover worker crash/reap, a bounded
partial-Reset timeout, stale-session IPC rejection, malformed partial HCI
delivery, and fresh IPC ownership after cleanup. Broker tests separately check
strict restore-state parsing and closure of the owned channel descriptor.

## Production invocation

The `sapphire-discovery` flake package supplies the pinned reactor and a narrow
`bluetooth-sapphire-discover` wrapper. Run it as root with absolute paths in
root-only directories:

```sh
bluetooth-sapphire-discover \
  --device 0 --seconds 6 \
  --report /var/lib/bluetooth-sapphire/discovery.json \
  --state /run/bluetooth-sapphire/controller.state
```

The wrapper always adds the explicit discovery-only confirmation. It also
forwards the operational forms `--probe-user-channel DEVICE` and
`--restore-controller-state ABSOLUTE`; use the latter from `ExecStopPost` as a
deadline guard. The runner package installs its baseline unit properties at
`share/bluetooth-sapphire/systemd.properties`. A service needs
`CAP_NET_ADMIN`, `CAP_NET_RAW`, `CAP_SETUID`, `CAP_SETGID`, and `CAP_KILL`, and
must allow `AF_UNIX` and `AF_BLUETOOTH`. Do not enable `PrivateNetwork` or
`PrivateDevices`: either prevents the required Bluetooth user-channel access.
Keep `RuntimeMaxSec=150s`, `TimeoutStopSec=5s`, `KillMode=control-group`, and a
root-only runtime directory, with for example:

```ini
ExecStopPost=bluetooth-sapphire-discover --restore-controller-state /run/bluetooth-sapphire/controller.state
```

Copy only the mode-0600 redacted report to durable root-only storage. This
path never writes peer addresses or names; do not persist raw HCI input.

`scripts/build-sapphire-gap-wasm` builds that suite from Pigweed commit
`c14c119c51a82f6e044f81b7dad0a322091d4121`, fetched by Nix from the upstream
Pigweed Gitiles archive with a fixed content hash. Pigweed and Sapphire retain
their upstream license notices. The script keeps Sapphire sources and tests
unchanged; its narrow build-platform patches select the Nix WASI toolchain,
replace unavailable host tool downloads, and provide the explicit bounded test
logging and exit imports in `upstream/wasi_test_stubs.c`.

Set `SAPPHIRE_GTEST_ARG` when building to bake one bounded GoogleTest argument
into a diagnostic artifact, for example `--gtest_list_tests` or
`--gtest_filter=AdapterTest.*`. This does not grant the guest ambient process
arguments; the generated test-only header contains the sole explicit value.
