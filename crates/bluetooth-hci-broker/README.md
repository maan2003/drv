# Bluetooth HCI transport oracle

This crate is a temporary Linux physical-transport oracle for the
Fuchsia/Pigweed Sapphire port. It exclusively opens an already powered-down
controller through Linux's HCI user channel, accepts only bounded HCI event
packets, performs time-bounded BR/EDR inquiry and LE scanning, and writes a
mode-0600 JSON report. It is not a product Bluetooth stack and deliberately
implements no pairing, L2CAP, ATT, GATT, SDP, or profile behavior.

The product boundary remains:

```text
sandboxed Sapphire process <-> bounded HCI packets <-> privileged broker
                                               Linux HCI user channel (temporary)
```

Sapphire must not receive the broker's descriptor or any USB, VFIO, firmware,
filesystem, or unrelated host authority. The WASM supervisor now routes its
bounded `controller.send` lowering through this crate's command, ACL, SCO, and
ISO frame validation into a quota-limited fake controller. Attaching
Sapphire's `bt::hci::Transport` and GAP discovery managers to inbound packet
delivery remains the next integration step. The deterministic decoder tests
here cover that boundary; Sapphire's upstream fake-controller discovery tests
remain the behavior oracle.

## Upstream inventory and provenance

`scripts/fetch-fuchsia-reference` pins:

- Fuchsia `1e1219e3fac944c9a906aea9646939746b6062b3`, including Bluetooth Rust
  services, profiles, integration tests, test harnesses, FIDL definitions, and
  the virtual HCI integration.
- Pigweed `c14c119c51a82f6e044f81b7dad0a322091d4121`, currently fetching
  `pw_bluetooth_sapphire` itself.

The first portable discovery targets are Sapphire's
`host/gap/{bredr,low_energy}_discovery_manager_test.cc`, backed by
`host/testing/{fake,mock}_controller`. The fetched Sapphire subtree alone is
not buildable: its Bazel targets use Pigweed support modules including
`pw_async`, `pw_bluetooth`, `pw_bytes`, `pw_chrono`, `pw_function`, `pw_log`,
`pw_random`, `pw_result`, `pw_span`, `pw_status`, `pw_string`, `pw_sync`, and
`pw_unit_test`, plus their transitive support and build rules. Fuchsia's
`bt_hci_virtual` additionally retains driver-framework, FIDL, and Zircon
plumbing that is not part of the portable controller model.

The smallest low-risk closure is therefore the pinned complete Pigweed source
tree with only Sapphire's host, fake-controller, and discovery test Bazel
targets selected. Trimming or rewriting that transitive closure before it
builds would make local code an unverified substitute for the accepted
upstream stack.

## Physical use

Invoke the binary as root with absolute `--report` and `--state` paths in a
root-only directory. It snapshots the controller flags before any change,
powers the controller down only when required for user-channel acquisition,
and restores the exact initial flags after closing the descriptor. The
`--restore-state` mode is idempotent and is intended for a systemd
`ExecStopPost` deadline guard. The binary always attempts scan disable and
inquiry cancellation before closing its descriptor. Keep reports and state
snapshots local because reports contain peer addresses and names; commit only
redacted counts.

## Verified physical slice

On `no-plastic`, the guarded oracle completed a six-second LE active scan and a
six-second BR/EDR inquiry through `hci0`. The root-only report contained three
unique LE peers (one public and two random addresses); no peer identity is
committed. The controller began and ended down with flags `0x00000000`, a
second exclusive user-channel acquisition succeeded after cleanup, and
`mt7921e`, iwd, SSH, and the disarmed Wi-Fi watchdog remained available. This
verifies only bounded nearby-device discovery through the temporary Linux
transport, not Sapphire execution, pairing, bonding, profiles, or sandboxing.
