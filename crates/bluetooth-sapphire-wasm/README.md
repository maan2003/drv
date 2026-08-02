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
