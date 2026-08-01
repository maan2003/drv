# Driver/Broker Component ABI v0

## Contract
`drv:hardware/broker@0.1.0` is the only host import of an untrusted no-WASI driver.
The host injects one authorized `device`; it cannot enumerate or name devices.
`resource` values are unforgeable capabilities, never FDs, pointers, or Linux
types. Integers are fixed-width; byte offsets/lengths are overflow-checked.
```wit
enum error { invalid, stale-handle, wrong-state, out-of-bounds, denied,
             limit, cancelled, device-fault, io }
enum device-state { quiescent, running, faulted, closed }
enum width { u8, u16, u32, u64 }
enum bar-kind { bar0, bar1, bar2, bar3, bar4, bar5 }
enum dma-access { device-read, device-write, bidirectional }
enum sync-for { device, cpu }
enum artifact-kind { firmware, nvram, signature, clm, txcap }
record range { offset: u64, length: u64 }
record limits { max-arenas: u32, max-dma-bytes: u64, max-batch-ops: u32, max-transfer-bytes: u32, max-wait-ns: u64 }
record region-info { kind: bar-kind, length: u64, windows: list<range>, widths: list<width> }
variant region-op { read(tuple<u64, width>), write(tuple<u64, width, u64>),
                    read-bytes(range), write-bytes(tuple<u64, list<u8>>) }
record batch-result { completed: u32, reads: list<list<u8>>, failure: option<error> }
record artifact-info { kind: artifact-kind, length: u64, sha256: list<u8> }
record irq-event { vector: u32, count: u64, at-ns: u64 }
resource device {
  state: func() -> device-state;
  generation: func() -> u64;
  limits: func() -> limits;
  regions: func() -> list<region-info>;
  open-region: func(kind: bar-kind) -> result<region, error>;
  alloc-dma: func(size: u64, alignment: u64, access: dma-access)
    -> result<dma-arena, error>;
  open-interrupt: func(vector: u32) -> result<interrupt, error>;
  artifacts: func() -> list<artifact-info>;
  open-artifact: func(kind: artifact-kind) -> result<artifact, error>;
  activate: func() -> result<_, error>;
  quiesce: func() -> result<_, error>;
  reset: func() -> result<u64, error>;
}
resource region { transact: func(ops: list<region-op>) -> batch-result; }
resource dma-arena {
  iova: func() -> u64; length: func() -> u64;
  read: func(ranges: list<range>) -> result<list<list<u8>>, error>;
  write: func(chunks: list<tuple<u64, list<u8>>>) -> result<_, error>;
  sync: func(ranges: list<range>, target: sync-for) -> result<_, error>;
}
resource interrupt {
  wait-until: func(deadline-ns: u64) -> result<option<irq-event>, error>;
  mask: func() -> result<_, error>; unmask: func() -> result<_, error>;
}
resource artifact { info: func() -> artifact-info; read: func(offset: u64, length: u32) -> result<list<u8>, error>; }
now: func() -> u64;
sleep-until: func(deadline-ns: u64) -> result<_, error>;
random: func(length: u32) -> result<list<u8>, error>;
```

## Semantics and safety
The initial state is `quiescent`: DMA is attached but interrupts are masked and
the device must not run. `activate` enables the prepared device; `quiesce`
masks interrupts and blocks DMA. `reset` performs the broker-selected function
or bus reset; it zeroes/frees all arenas and returns a fresh generation in
`quiescent`. Reset, worker exit, watchdog cancellation, or broker reconnect
revokes every subordinate resource; every
old call returns `stale-handle`. A failed reset leaves `faulted`.
BAR capabilities cover only broker-approved ranges. Access is naturally
aligned, little-endian, and bounded; batches are validated before execution,
ordered, non-atomic, and report the completed prefix; reads are returned in
read-op order. `u64` is offered only when the region supports it. BARs are never
mapped into Wasm memory.
Each arena is zeroed, broker-allocated memory mapped once at a broker-selected
IOVA with least DMA permissions. Only its IOVA and bounded copied bytes cross
the ABI; Wasm linear memory is never DMA mapped. `sync(..., device)` publishes
prior CPU writes before a doorbell; `sync(..., cpu)` completes device writes
before reads. The t8112 PCIe path is declared DMA-coherent, so v0 implements
these as ordering fences; a non-coherent host must implement cache maintenance
or reject the device. No concurrent calls may touch overlapping arena ranges.
Interrupt counts coalesce notifications and never expose eventfds. Waits use
broker monotonic nanoseconds, are bounded by `max-wait-ns`, return `none` at the
deadline, and return `cancelled` when the supervisor stops the worker. Wall
clock, threads, filesystem paths, environment, and other WASI APIs are absent.
`random` is a quota-limited host CSPRNG needed for the firmware boot seed.
Firmware artifacts are immutable broker-selected blobs addressed only by
logical kind; reads and hashes are bounded by the advertised manifest.
Calls and batches are quota-limited. Use bulk region/DMA copies for firmware and
frames, and one ordered transaction for register sequences; zero-copy and BAR
mapping are intentionally deferred.

## Broker-only and unresolved
The broker alone owns sysfs discovery, PCI binding/config space and power,
VFIO/iommufd FDs and ioctls, group ownership (including companion Bluetooth),
IOAS/IOVA selection and map/unmap, BAR mmap, eventfds/MSI setup, reset choice,
page pinning/cache maintenance, artifact path/selection/trust, policy, quotas,
logging, and worker creation/termination.
**Unresolved for v0 implementation:** exact BCM4387 BAR range/width allowlist;
arena sizes, alignments, IOVA width, and batch limits; whether signature/hash
verification is policy or merely inventory; reset scope and recovery when the
shared upstream reset affects Bluetooth; and whether Component Model async
futures replace bounded blocking waits. Non-coherent DMA is not supported until
the required DART/Linux cache-maintenance mechanism is proven.
