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

The first executable target is the pinned upstream Sapphire fake-controller
GAP discovery suite. Its WASM import list must match the interfaces in the WIT
world before it can be connected to the physical HCI broker.
