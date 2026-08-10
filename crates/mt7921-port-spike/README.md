# MT7921/MT7922 port spike (not yet a driver)

This crate is an exploratory port of two hardware-independent format seams from
Linux mt76: the 16-byte DMA descriptor construction and the Connac2 RAM firmware
trailer/region parser. It does not access hardware, load firmware, implement
802.11, or claim support for any device. Its purpose is to make a small amount
of source-corresponding code compile while exposing what the current hardware
broker cannot yet express.

The BCM4387C2 first target in `specs/ARCH-asahi-wifi-target.md` is unchanged.

It also contains the first portable SoftMAC seam: `AccessPoint::from_beacon`
ports the behavior of pinned Fuchsia's
`mlme/rust/src/client/convert_beacon.rs::construct_bss_description` without its
FIDL types. It consumes raw beacon/probe-response information elements and
produces the BSSID, SSID, channel, signal, capabilities and security summary
needed by the host. Its main fixture is copied from Fuchsia's corresponding
test. Security AKM classification follows the suite handling in Fuchsia WLAN
common/SME protection code. Fuchsia sources are BSD-3-Clause licensed; the
ported implementation was rewritten against the pinned source rather than
copied with component-runtime dependencies.

The `mt7921-scan` binary is an explicitly temporary Linux SoftMAC adapter. It
converts `iw scan` text into newline-delimited JSON so the portable result
shape and durable physical evidence can be exercised before the VFIO WFDMA/MCU
transport exists. Its output proves real RF enumeration but **does not** prove
userspace firmware initialization, DMA, IRQ, or scan operation.

`mt7921-vfio-read` is the first physical MT7921 transport slice. It validates
the exact no-plastic PCI and subsystem IDs, attaches the VFIO cdev to a fresh
iommufd IOAS, and maps only two 4 KiB BAR pages read-only. Calls can read only
five enum-selected registers: MCU state, host interrupt status, WFDMA global
configuration, PCIe ownership synchronization, and firmware power/readiness.
The offsets come from pinned Linux `mt792x_regs.h` and the fixed map in
`mt7921/pci.c`. It cannot write MMIO, remap arbitrary chip addresses, allocate
DMA, arm an interrupt, or reset the function.

With `--acquire-driver-ownership`, the same binary additionally ports
`__mt792xe_mcu_drv_pmctrl` from pinned Linux `mt792x_core.c`: it writes only
`PCIE_LPCR_HOST_CLR_OWN` to `MT_CONN_ON_LPCTL`, then polls only that register's
`PCIE_LPCR_HOST_OWN_SYNC` bit. It preserves Linux's ten 50 ms attempts and 1 ms
poll tick while adding a 500 ms absolute deadline and rejecting command bits
on readback. Every write, status sample, retry, terminal success, timeout, or
unexpected state is emitted as a structured event. No firmware-ownership or
dynamic L1-remap write is admitted.

The physical MT7961 at `0000:05:00.0` completed this transition on the first
attempt: writing `PCIE_LPCR_HOST_CLR_OWN` produced a zero status response and
driver ownership at 0 ms. The bounded run then observed firmware power set,
N9 readiness clear, and all TX/RX DMA enable/busy bits clear before issuing a
VFIO function reset. The root-only report is
`/var/lib/wifi-driver-lab/reports/20260802T170812Z-0000_05_00.0.log`.

## Inactive WFDMA and firmware prerequisites

`WfdmaRing` ports the inactive mt76 TX-ring invariants without enabling DMA:
descriptor storage must be aligned and wholly below 4 GiB, reset descriptors
are CPU-owned via `DMA_DONE`, enqueue writes a device-owned descriptor before a
release fence and producer-index publication, reclaim requires device
completion and an acquire fence, indices wrap within the allocation, and
teardown resets descriptors before releasing the allocation. The model has no
register or DMA-enable operation.

`Patch` adds bounds-checked parsing of the big-endian Connac2 patch header and
section table consumed by pinned Linux `mt76_connac2_load_patch`.
`mt7921-firmware-inspect` reads and decompresses only the exact installed
MT7961 patch/RAM artifact names, validates their target metadata and every
payload bound, and emits structured metadata. It does not retain, map for DMA,
or send firmware bytes.

`--read-dynamic-identity` admits the minimum dynamic-L1 selector operation used
by pinned Linux `mt7921_reg_map_l1`, but closes over four read-only targets:
`MT_HW_CHIPID`, `MT_HW_REV`, `MT_HW_BOUND`, and `MT_TOP_LPCR_HOST_BAND0`.
It saves the selector, selects only bases `0x7001` and `0x1806`, verifies each
posted selector write, reads only the enum-selected offsets through a separate
read-only window mapping, and restores the original selector on success or
failure. This mode cannot write the dynamic window or request MT_TOP ownership.

`--acquire-top-ownership` separately ports pinned Linux
`mt7921e_driver_own`. It selects only `MT_TOP_LPCR_HOST_BAND0`, writes only
`MT_TOP_LPCR_HOST_DRV_OWN`, and polls `MT_TOP_LPCR_HOST_FW_OWN` clear with a
500 ms hard deadline and 1 ms ticks. Command-bit readback is rejected, every
transition is logged, and the saved remap selector is restored on every exit.

`--program-disabled-fwdl-ring` allocates one anonymous page, maps it through
iommufd at fixed low-32-bit IOVA `0x01000000`, initializes the first 128
descriptors to CPU-owned `DMA_DONE`, and refuses to continue unless WFDMA TX/RX
enable bits and the complete host interrupt-enable register are zero. It then
temporarily programs only firmware-download ring 16's descriptor base, count,
and CPU index; verifies readback including the untouched DMA index; restores
the original ring registers; explicitly unmaps the complete arena; and emits
an event for every step. Restoring those visible resources did not make the
kernel fallback usable in the first physical run, despite a successful script
and supervisor restoration; the machine required a cold power cycle. The mode
therefore now requires VFIO reset capability and issues `VFIO_DEVICE_RESET`
after arena unmap and before returning the function to the supervisor. It
cannot write WFDMA enable, interrupt, or DMA-index registers. Any repeat must
run through the externally renewed `wifi-driver-lab-remote` reboot watchdog.

`reset_wfsys` also ports the device-specific recovery sequence from pinned
Linux `mt792x_wfsys_reset`: clear `WFSYS_SW_RST_B`, hold for 50 ms, set it, and
poll `WFSYS_SW_INIT_DONE` for at most 500 ms. No physical adapter for address
`0x18000140` is admitted yet; deterministic success and timeout behavior must
precede that additional dynamic-L1 write surface.

`--mask-ack-disabled-fwdl` ports the ring-16 subset of pinned Linux interrupt
handling. It refuses active DMA or a nonzero host mask, snapshots status,
writes only the zero mask, acknowledges only `HOST_TX_DONE_INT_STS16` using
W1C semantics, verifies readback without clearing unrelated sources, restores
the zero mask, and performs the mandatory VFIO reset. It does not arm a VFIO
IRQ or enable any device interrupt.

`--stage-disabled-firmware-descriptor` decompresses and bounds-checks the exact
installed MT7961 ROM patch, maps separate one-page descriptor and payload
arenas at fixed low-32-bit IOVAs, and stages only its first 4096-byte raw
`FW_SCATTER` chunk. It first requires TX/RX DMA and every host interrupt to be
disabled. The payload is copied before a release fence publishes one ring-16
descriptor, which is read back and then reset to CPU ownership; the payload is
zeroed, both mappings are explicitly removed, and VFIO reset is mandatory.
No ring register, producer index, DMA-enable bit, interrupt mask, or MCU command
is written, so the device cannot observe the staged descriptor.

The offline patch-protocol slice now derives Linux's Connac2 download mode
from each parsed section security word and encodes the `PATCH_FINISH_REQ` that
must follow all scatter chunks. Unknown encryption modes fail closed. These
helpers are fixture-tested only and are not connected to MMIO or active DMA.
The same pure encoder now includes the terminal `FW_START_REQ` used after the
exact installed RAM regions, rejects an address/option pair other than their
derived `0x00915000`/override values, and emits Linux's required legacy command
queue ID (`0x8000`) in every command TXD.
`firmware_download_mode` separately ports the Connac2 RAM-region feature-byte
translation, including encryption, key index, encryption mode, response, and
optional CR4 working-PDA bits. Address override and non-download remain region
flow controls rather than download-mode bits.

`load_mt7921_firmware` composes those pure pieces behind a typed transport. It
powers the NIC, bounds the download-ready poll, and always attempts release of
an acquired patch semaphore. It initializes and scatters every patch section in completed
chunks of at most 4096 bytes, finishes the patch, downloads only RAM regions,
starts the exact installed image, and bounds the N9-ready poll. Cleanup runs
after success and every injected failure, preserving both primary and cleanup
errors when necessary. Golden-trace and per-operation error-injection tests
cover the transaction. `VfioFirmwareLoader` is the bounded physical adapter: it
uses the fully owned command/FWDL/RX rings, matched MCU responses, modular DIDX
completion, cancellation, IRQ disable, DMA quiescence, and reset-while-pinned
teardown before any mapping is released. `--run-one-shot-fwdl` is the explicit
lab-only entry and must run through `wifi-driver-lab-remote`'s reboot watchdog.
Like pinned Linux, a one-second download-ready timeout is recorded as a warning
and loading continues; N9 readiness remains a terminal 1.5-second timeout. The
offline safety model is stricter than Linux scatter submission: it requires an
explicit completion for every chunk under a three-second deadline. Transport
sequence allocation persists across transactions and skips zero on four-bit
wrap. Fail-closed cleanup after `Ready` is lab transaction policy; Linux keeps
the live device resources instead. The operation stops immediately after clean
N9 readiness and sends only the read-only `GET_NIC_CAPAB` and source-exact
`EFUSE_ACCESS` query for the 16-byte EEPROM block containing `MT_EE_HW_TYPE`
before cleanup.
The bounds-checked response parser exposes MAC, PHY stream/band, 6 GHz, and chip
capability TLVs while retaining the element/unknown counts. It rejects truncated
headers, values, and undersized known elements. No radio, channel, regulatory,
or scan command is encoded or sent. The EEPROM response is bounded, matched to
address `0x550`, and exposes byte `0x55b` bit 0 as the calibration-enclosure
selector used by pinned Linux. The local non-download CLC firmware region is
then inventoried without sending `SET_CLC`: segment/rule bounds, first-record
selection, duplicate country rules, and the `00` fallback domain are retained.
This read-only boundary can derive physical band availability from NIC caps and
the calibration/regulatory catalog supported by the exact hardware artifact.
It deliberately cannot claim a final valid-channel set: pinned Linux treats CLC
rule data as opaque, and per-channel legality additionally requires the
mutating `SET_CLC` response, cfg80211 country regdb, and any OF/DTS limits.
The source-exact `SET_CLC` wire format is available only inside the explicit
one-shot loader gate: it selects every opaque `00`/indoor rule from the accepted
installed CLC record and encodes CID
`0x5c`, preserves Linux's no-ACPI `MTCL_INVALID` sentinel as `0xff` in the
packed request, and bounds the 68-byte response plus five-bit UNII mask. State advances
to `ClcConfigured` before publication so any timeout or response mismatch still
forces reset-while-pinned cleanup.

`SET_CHAN_DOMAIN` is a second, separately selected one-shot boundary. Its
source-exact packed Connac2 request uses CID `0x0f`, bandwidth fields `0/3/3`,
and eight-byte little-endian channel records, with Linux's `wait_resp=false`
TX-completion contract. The request can only be generated for country `00`,
indoor operation, and a firmware special-UNII mask of zero. It intersects the
device capability with the pinned Fuchsia passive universe: 2.4 GHz channels
1-14 and 5 GHz channels 36-165 (excluding 169-177), never 6 GHz. All 39 channels
in the installed capability fixture carry `IEEE80211_CHAN_NO_IR`, so this step
does not authorize transmission. The old `--run-one-shot-fwdl` mode still stops
after CLC; only `--run-one-shot-channel-domain` can publish the new command.
Both modes retain simultaneous WM/WM2 receive ownership and mandatory
reset-while-pinned cleanup. No set-channel, radio-enable, or scan command is
encoded by this boundary.

One watchdog-guarded physical run completed the channel-domain boundary from
commit `8b7f6dac` (release SHA-256
`f2a335e253383d679903ee1951e02bad4a1a76847a75e3ed7aad94176b45b58d`).
The dual-ring CLC response returned special-UNII mask zero, then sequence 15
published exactly 39 world/indoor `NO_IR` channels and reached TX completion.
The run stopped there, reset while pinned, released every DMA mapping, and
returned success; the supervisor restored `mt7921e` and iwd with no restore
failure. No set-channel, radio, or scan command followed. The mandatory
watchdog reboot restored boot `361cd83b-d30d-4f3c-abdd-2954758326c2` with
`wlan0` up, its default route present, iwd active, and `failed=0`. The durable
report is
`/var/lib/wifi-driver-lab/reports/20260809T143339Z-0000_05_00.0.log`.

The next offline slice inventories the smallest pinned-Linux passive-scan
closure after channel-domain configuration. It encodes EFUSE buffer mode, MAC
enable, 2x2 `SET_RX_PATH`, unified device/BSS activation, an other-BSS
management receive filter, passive off-channel tuning, and one-channel
`START_HW_SCAN`. The Connac2 scan request is fixed to passive type, zero SSIDs,
zero probe requests, zero IEs, no random MAC, and no general transmit API; its
only scan function bit is Linux's split-scan bit. Typed parsers accept only the
matching unsolicited scan-done event and normal-RX beacon/probe-response
envelopes. Data/control frames, translated headers, RX errors, missing P-RXV
RSSI, and channels outside 2.4 GHz 1-14 fail closed. TX-only RTS/SAR/LED setup
is deliberately outside this receive-only closure.

`mt7921-softmac-adapter` now drives these exact encoders through the real
pinned Fuchsia `SoftmacHardware` implementation. It retains Fuchsia scan IDs,
bounded requested timing, advertisement conversion, and completion semantics.
The lower mechanics edge must attest that the mask-zero channel domain,
source-required MAC MMIO initialization, and separately owned data-RX ring are
all present before MAC enable or channel configuration. Pinned Linux first
sends `EFUSE_BUFFER_MODE`, then performs `mt7921_mac_init`; the adapter preserves
that order and poisons on a failed prerequisite before any later command. This
remained offline-only until the
physical mechanics edge owns RX ring 2/interrupt bit 2 and ports the mandatory
MAC MMIO sequence without weakening the existing dual-MCU-ring cleanup gate.

`mt7921-passive-scan` now supplies that isolated GPL physical edge for one
explicit channel-1 gate. It owns an independent eight-entry RX ring 2, verifies
all four ring registers before authorizing interrupt bit 2, and executes the
ordered 41-operation MAC plan with pinned Linux's MMIO semantics. It then runs the source-exact transport
through pinned Fuchsia `SoftmacHardware`, accepts only parsed beacon/probe
observations plus the matching scan-done event, and requires both a successful
completion and at least one BSS. Its DMA mappings join the existing mandatory
reset-while-pinned cleanup. The channel-1 gate is now physically validated as
described below; broader channel coverage remains outside that result.

The first watchdog-contained physical attempt from commit `301cdcec` (release
SHA-256
`c8cd0b0a8c8278af1a8f0860bc7f8c8cb7ffae8f5c4b18ba2e8592d6580b00cb`)
stopped before the passive boundary. It had unmasked data-RX interrupt bit 2
during the firmware bootstrap and then rejected ring 0's first RX envelope as
`InvalidLength` while waiting for `PatchSemaphoreGet`. Status `0x08000001` was
not itself novel: pinned Linux identifies bit 27 as TX-ring-17 MCU completion
and bit 0 as WM RX, and both prior successful channel-domain runs observed that
same status before draining the NIC-power event and semaphore response. Cleanup
disabled DMA, reset while pinned, released all mappings, and supervisor restore
reported `failed=0`; the armed watchdog rebooted to healthy boot
`32459d23-ec4c-4455-9d12-392763f8307f`. The durable report is
`/var/lib/wifi-driver-lab/reports/20260809T151850Z-0000_05_00.0.log`.

The offline follow-up restores the previously proven WM/WM2-only interrupt
mask throughout bootstrap. Ring 2 remains prepared and readback-verified, but
bit 2 is unmasked and verified only inside `prepare_passive_receive`, after the
loader reaches the channel-domain boundary and the MAC plan completes. Parse
failures now retain descriptor control, descriptor length, and MCU header
length for a future authorized diagnostic run. No retry has validated this
fixture-backed ordering fix, so physical passive reception remains unproven.

The narrower `--run-one-shot-passive-prepare` diagnostic is the only next
physical gate. It retains the proven WM/WM2-only bootstrap through firmware,
CLC, and channel-domain setup; then, inside the mandatory passive hook, it
executes the MAC plan, replaces inert RX slot 2 with its dedicated descriptor
ring, verifies base/count/CPU/DMA words, authorizes and enables bit 2, verifies
the interrupt mask, and stops. It emits no device/BSS/channel-switch/scan MCU
commands and therefore performs no radio dwell. Each ordered prepare step has
failure-injection coverage, while the loader fixture verifies passive-hook
failure still reaches mandatory cleanup.

The single prepare-only physical run from commit `e7d92d2c` (release SHA-256
`d416c420917c2a7c682d92ad8007194a94d1e76894492cecb9f9191fe5e10d6c`)
proved the restored bootstrap ordering: firmware, CLC, and channel-domain setup
completed under IRQ mask `0x00400001`. The hook then stopped on its first MAC
read because the attempted L1-remap path returned all ones for `0x820cd004`.
Ring 2 and bit 2 were therefore untouched. Cleanup/reset/unmap and supervisor
restore succeeded, and the watchdog rebooted to healthy boot
`07997fa0-bcf0-46d8-b3bc-32aaf9b4c49f`. The report is
`/var/lib/wifi-driver-lab/reports/20260809T152843Z-0000_05_00.0.log`.

Pinned `__mt7921_reg_addr` explains the failure: every address in the mandatory
MAC plan matches a fixed-map entry before the L1 fallback. In particular,
`0x820cd004` translates through `0x820cd000 -> BAR 0x0f000` to BAR offset
`0x0f004`; changing `MT_HIF_REMAP_L1` was incorrect. The offline executor now
uses exact fixed-map fixtures for every plan address, maps only the eight BAR
pages those fixtures require, rejects addresses outside the plan, and treats
all-ones reads as typed failures. It does not touch or restore the remap
selector; the existing top-ownership transaction remains responsible for its
own earlier save/restore. This correction was not yet physically validated at
that point, so the next hardware milestone remained prepare-only.

The fixed-map prepare retry from commit `a9ef2fcb` reached the MAC plan but
stopped at `MT_WF_RMAC_MIB_TIME0(0)`: its bit-30 enable write was observed as
zero on an immediate read. Report
`/var/lib/wifi-driver-lab/reports/20260809T153856Z-0000_05_00.0.log` records the
clean reset/restore and watchdog boot `aee94d73-c729-495d-be53-5abed2552b29`.
A source-order correction then sent `EFUSE_BUFFER_MODE` immediately before MAC
initialization, matching `__mt7921_init_hardware`, but report
`/var/lib/wifi-driver-lab/reports/20260809T154715Z-0000_05_00.0.log` observed
the same value and again stopped before ring 2/IRQ2.

Pinned `mt76_mmio_rmw` explains why equality was the wrong verification
contract: it performs one `readl`, calculates `value | (initial & ~mask)`,
performs one `writel`, and returns the calculated value without requiring an
immediate hardware readback. `MT_WF_RMAC_MIB_RXTIME_EN` is an enable bit, not a
documented write-one/self-clearing field, but pinned initialization still does
not use its immediate read value as success evidence. The executor therefore
retains all-ones rejection on both pre- and post-write reads and durably logs
initial/programmed/observed values, while accepting the source primitive's
single-read/write completion. WTBL update remains different: its pinned source
explicitly polls the busy bit, so that bounded verification remains mandatory.

The source-semantics prepare run from commit `3dcae792` (release SHA-256
`cad2920e1e176b13c59803fc6cf4d68d34a57428c34ba8c3183f72cf9dc79c52`)
passed the complete non-radio gate. All 41 fixed-map operations executed; both
WTBL busy polling and all-ones rejection remained active. Ring 2 programming
and its four-word readback passed, interrupt bit 2 was authorized/enabled and
the full mask read back, then the hook stopped without channel or scan
commands. Cleanup/reset/unmap returned success and supervisor restore reported
`failed=0`. The watchdog rebooted to healthy boot
`2ea5e790-451a-48dc-b277-0e0a8a37e8be`. The durable report is
`/var/lib/wifi-driver-lab/reports/20260809T155035Z-0000_05_00.0.log`.

The first full channel-1 attempt then exposed a bounded host-side allocation
error after MAC enable: the 256-entry MCU TX ring had only 4 KiB of command
payload space, enough for descriptor slots 0-15. Report
`/var/lib/wifi-driver-lab/reports/20260809T155608Z-0000_05_00.0.log` stopped
before `SET_RX_PATH`. Commit `f38fc7ce` provisions a non-overlapping 64 KiB
arena for all 256 256-byte descriptor offsets. The next attempt reached and
completed `START_HW_SCAN`, then report
`/var/lib/wifi-driver-lab/reports/20260809T160535Z-0000_05_00.0.log` rejected a
181-byte ring-4 packet because it was not an MCU event envelope. A diagnostic
retry in report
`/var/lib/wifi-driver-lab/reports/20260809T161149Z-0000_05_00.0.log` identified
Connac2 packet type 7 with flag 1. Pinned `mt7921_queue_rx_skb` normalizes that
exact combination to normal data, so commit `961c038b` routes it to the same
strict beacon/probe parser while all other malformed MCU envelopes still fail
closed.

The resulting watchdog-contained run passed the complete one-channel physical
gate from release SHA-256
`8f0b47cbf751df35f1d72c48c6d808d00307b76ad9c724040e032e84c78539aa`.
It completed every source-exact setup command, received one channel-1 beacon at
-61 dBm through WM2 ring 4, matched unsolicited scan-done event `0x0d` to scan
ID 1, and emitted `one_channel_gate_passed`. Cleanup disabled bus mastering,
reset while every IOVA remained pinned, released every mapping, and returned
success; supervisor restoration also returned `failed=0`. The watchdog was
disarmed without reboot. On unchanged boot
`34c7207e-885b-4d1c-987c-e691c079285c`, `mt7921e` and iwd were active and
`wlan1` held the default route. The durable report is
`/var/lib/wifi-driver-lab/reports/20260809T161754Z-0000_05_00.0.log`.

A subsequent bounded gate reused the same initialized adapter for sequential
passive dwells on channels 1 and 6. Release SHA-256
`dde2f8f05a5d93bf6a9d1d09a32c238ba7845db1a7441d6f3c2fd818ddf921c2`
matched both scan completions and accepted one beacon on each channel before
emitting `sequential_gate_passed`. Cleanup, restoration, and watchdog disarm
all succeeded without reboot; `mt7921e`, iwd, and the default route remained
healthy on the same boot. The durable report is
`/var/lib/wifi-driver-lab/reports/20260809T162215Z-0000_05_00.0.log`.

The next gate completed sequential passive dwells across every 2.4 GHz channel
1-14 from release SHA-256
`4535b5ae1af9544a5bfab3033a3873d17d7241aba42f0073c096d6c1b468ea43`.
All 14 scan IDs received matching successful completion and three valid beacon
observations were delivered overall. Cleanup and supervisor restoration both
returned success. SSH dropped during the longer handoff, so the client did not
retry and the armed watchdog rebooted as designed; boot
`5e7097d2-4971-4fa1-a9fd-ffa2a1cc8173` returned with `mt7921e`, iwd, `wlan0`,
and its default route healthy. The durable report is
`/var/lib/wifi-driver-lab/reports/20260809T162423Z-0000_05_00.0.log`.

The first bounded 5 GHz group covered non-DFS world/indoor channels 36, 40,
44, 48, 149, 153, 157, 161, and 165. The source-exact encoders preserve passive
scan type, zero SSIDs/probes/IEs/random MAC, and the existing `NO_IR` channel
domain while selecting Linux's 5 GHz band fields. Release SHA-256
`82289992d61a5cfd75947c4a15ffbb9c54ad2d6f945db49d6a7a2f7df950768d`
received matching completion on all nine channels and one valid channel-36
beacon. Cleanup and restore returned success; after the watchdog reboot,
`mt7921e`, iwd, `wlan0`, and its default route were healthy on boot
`1b50ae57-836e-4a88-9600-2a2692e5702d`. The durable report is
`/var/lib/wifi-driver-lab/reports/20260809T163223Z-0000_05_00.0.log`.

The low-DFS passive group covered channels 52, 56, 60, and 64 under the same
world/indoor `NO_IR`, zero-probe contract. All four channels returned matching
successful completion with no local BSS, which is a valid empty scan result.
Release SHA-256
`df16b13016bf951a0e6d2c7a3db0dcfc0b36c7d7fde231b66d9e4ddbd8085d92`
emitted `sequential_gate_passed`; cleanup and restoration returned success,
and watchdog boot `3ea2362d-a0f7-4ac0-9313-8a0a6fbf8581` returned with native
Wi-Fi and its default route healthy. The durable report is
`/var/lib/wifi-driver-lab/reports/20260809T164050Z-0000_05_00.0.log`.

The high-DFS group completed the remaining world/indoor channels 100, 104,
108, 112, 116, 120, 124, 128, 132, 136, 140, and 144. Release SHA-256
`681576803a43c7d9d9a30c4c6cc0cde190bb6ded70565394772089ce36bc137b`
received matching successful completion on all 12 channels and correctly
returned an empty BSS set. Cleanup and restoration returned success; watchdog
boot `febfd4af-29ef-4f72-8304-8ab6cfb73671` restored native Wi-Fi and its
default route. This completes bounded physical coverage of all 39 channels in
the mask-zero world/indoor `NO_IR` domain. The durable report is
`/var/lib/wifi-driver-lab/reports/20260809T164539Z-0000_05_00.0.log`.

The source-exact transport now accepts one multi-channel Fuchsia hardware scan
request and implements it as sequential single-channel firmware scans under a
single device scan ID. It retains the strongest observation for each BSSID,
sorts the aggregate by receive timestamp, then delivers deduplicated results
and the final completion through pinned Fuchsia `PassiveScanner`; cancellation
clears remaining channels and buffered results before sending the source-exact
cancel command. No regulatory decisions are added here: the full physical gate
obtains its 39-channel list from pinned Fuchsia `allowed_passive_channels`
under the already-attested world/indoor, special-UNII-zero policy.

Release SHA-256
`d9df33caa3fe58ee318b1e7e04b3ab2b0c03f5f4407dbe2b938ba71f3bbf8586`
completed all 39 sequential dwells as one SME transaction, delivered four
deduplicated BSS results with transaction ID 1, and emitted a successful scan
end. Cleanup and restoration returned success. The watchdog rebooted after the
expected SSH loss; boot `0a4aa41e-1202-4228-bc1b-a0a0738d214d` restored
`mt7921e`, iwd, `wlan0`, and its default route. The durable report is
`/var/lib/wifi-driver-lab/reports/20260809T165608Z-0000_05_00.0.log`.

The no-frame power setup gate then bound rate-power programming to a fresh,
exact channel-36 direct-beacon observation for the configured BSSID and SSID.
The first run failed closed after a source-exact scan returned no observation;
report `/var/lib/wifi-driver-lab/reports/20260809T182415Z-0000_05_00.0.log`
has SHA-256
`a337221f451f6b2cbeb50c81dc43d74c1098c8dc73b4b5c480a9ecb22ce77073`.
A bounded follow-up proved that a fresh second scan could authorize the target,
then exposed the incorrect use of `MCU_EVENT_ACCESS_REG` (`0x02`) for the
legacy CE register response; report
`/var/lib/wifi-driver-lab/reports/20260809T183545Z-0000_05_00.0.log` has
SHA-256
`cd4f36e96a842beec26acb7f5a58667f572d6817932e99dbd5d4f7d2ed71b271`.
Both runs reset and restored cleanly before any frame-publish path.

Release SHA-256
`a65b97d2d8d8484fc4dfb71fbd8e6489f533eee95f0afba23756dd7246eefe75`
completed the corrected setup gate. It authorized the exact direct ESS beacon,
consumed all eight conservative rate-power batches, and accepted each
sequence-correlated, solicited `MCU_EVENT_REG_ACCESS` (`0x05`) response with
reported length 20 and exact `MT_PSE_BASE`. It emitted
`no_frame_gate_passed` with beacon authorization and rate-power consumption
true and management-frame publication unreachable. Cleanup, supervisor
restore, native `mt7921e`/iwd reconnection, and watchdog disarm all succeeded
without reboot. The durable report is
`/var/lib/wifi-driver-lab/reports/20260809T184307Z-0000_05_00.0.log` (SHA-256
`2afbe77624cfb25da68c52223891fbcf39aeb20193aa65883c2d77b93b6dfe21`).

Pinned PCI Linux changes normal post-N9 MCU responses to the WM2 receive queue.
It nevertheless keeps both WM ring 0 (interrupt bit 0) and WM2 ring 4
(interrupt bit 22) allocated, enabled, and drained. The exact installed
no-plastic artifact and EFUSE/NIC-capability fixture selects one rule:
segment 0, country `00`, type `2d30` (`-0`), 482 opaque bytes, request length
622, capability 1. Linux therefore passes `wait_resp=true` for that rule and
sends it on `MT_MCUQ_WM` (WFDMA TX ring 17, TXD Q index `0x20`). A capability
without `CLC_CAP_EVT_EN` instead passes `wait_resp=false`; enqueue success
advances to the next rule without an IRQ or response, leaving the UNII mask
unchanged. `mt7921-firmware-inspect` emits this rule-by-rule fixture directly
from the installed artifact. The VFIO adapter now owns, enables, acknowledges,
and drains both receive rings concurrently, with independent producer wrap and
sequence matching; choosing either ring in isolation is not source-equivalent.
This path is release-built and fixture-tested but has not received a new live
authorization.
The focused containment review verifies that both descriptor/buffer arenas are
initialized before bus mastering, the active MMIO allowlist admits exactly RX
bits 0 and 22 (not the TX-done bit 27), IRQ handling masks then acknowledges
only those asserted sources and rearms both rings, and every exit masks host
interrupts, disables DMA and bus mastering, disables VFIO IRQ delivery, resets
while all mappings remain pinned, then attempts teardown of every arena even
after an injected unmap failure.
The stock driver exposes only `mt76` register/IRQ/TX-done tracepoints and
current debugfs queue counters on no-plastic; `fw_debug` is disabled and the
boot log contains firmware identity but no CLC command envelope or completion.
Those read-only surfaces cannot reconstruct the probe-time CLC transaction.
Capturing it would require pre-arming additional function/kprobe or firmware
logging and then retriggering probe/regulatory work, so it remains behind a new
explicit live-device gate.

A watchdog-guarded physical run completed this boundary for the exact installed
MT7961 artifacts. It downloaded one patch section and four RAM regions in 196
individually completed chunks (795,264 bytes), skipped the CLC region, reached
N9 `Ready`, switched to WM2 ring 4, and received a 528-byte capability response.
The typed result reports 23 elements, MAC `50:5a:65:f6:f9:89`, HT/VHT/HE,
5 GHz, two spatial streams, no 6 GHz, chip capability 19, and 19 preserved
unknown elements. A later guarded extension received the 24-byte EFUSE payload
on WM2 ring 4 (event `0xed`): address `0x550`, `valid=0`, and zero data, so the
source-consumed hardware-enclosure bit is clear. The installed CLC artifact has
one selected power segment, 196 rules, 152 unique country codes including
`00`, and no separate channel segment. Combined with NIC caps, the exact mt76
candidate universe is 14 2.4-GHz and 28 5-GHz channels, with no 6-GHz
candidates; regulatory validity remains intentionally unknown. It then disabled
DMA and PCI bus mastering, reset while all mappings were pinned, and released
every mapping. The lab restored `mt7921e`,
iwd, network, and SSH with `failed=0`. The durable root-only report is
`/var/lib/wifi-driver-lab/reports/20260809T124531Z-0000_05_00.0.log`.

A later single watchdog-guarded run validated the simultaneous Linux receive
contract. Post-N9 commands kept WM ring 0/interrupt bit 0 and WM2 ring
4/interrupt bit 22 active together (`0x00400001`). Capability event `0xec` and
EFUSE event `0xed` arrived on ring 4, followed by the solicited `SET_CLC` event
`0x80` for sequence 14 on ring 4. The bounded CLC response parsed successfully,
one world/indoor rule was applied, and the special-UNII mask was zero. The run
stopped before channel-domain, radio, channel, or scan operations, reset while
all mappings remained pinned, released every mapping, and restored `mt7921e`
and iwd with `failed=0`. The mandatory watchdog reboot then restored a healthy
network on boot `3aa1bf1a-1634-41bf-aba5-973ee65d14df`. The durable report is
`/var/lib/wifi-driver-lab/reports/20260809T141820Z-0000_05_00.0.log`.

The earlier `--run-one-shot-fwdl` rejection identified that the
global TX-DMA enable can fetch every TX ring, including stale kernel ring bases,
and that raw patch scatter is invalid until the MCU has accepted patch
semaphore and `PATCH_START` commands. Active DMA therefore remains unavailable
until the backend owns or guards every TX ring, resets and verifies every DMA
index, installs a VFIO IRQ before unmasking it, implements the MCU command/RX
response path, and keeps every mapping pinned through quiescence and function
reset. The bounded VFIO adapter now satisfies those gates for this explicit
operation; other commands cannot enter its active DMA path.

`prepare_global_tx_rings` is the deterministic replacement preflight. While TX
DMA and all host interrupts remain disabled, it inventories all 18 hardware TX
ring slots, rejects invalid MMIO or any `CIDX != DIDX`, verifies MT7921 ring
16's pinned-Linux prefetch value `0x03400004`, and replaces every non-target
base with one pinned guard page while assigning separate pinned backing to ring
16. Only after every base/count/CPU index is owned does it issue Linux's global
DTX-index reset and require every DIDX to read zero. Old kernel DMA bases are
never restored. Its fake transport remains the exhaustive failure-path model;
the physical inactive adapter below now covers the successful MMIO path.
Ring 17 now receives its own 256-descriptor MCU-command page rather than guard
backing. `prepare_mcu_rx_ring` separately builds Linux's eight-entry,
2048-byte-buffer pre-firmware response queue with seven device-owned buffers
and one empty slot, using a distinct aligned low-32-bit ring page and 16 KiB
buffer mapping. It rejects overlapping or out-of-range arenas.
`program_disabled_mcu_rx_ring` requires that old ring zero is idle, writes its
owned base/count with both CPU and DMA indices zero, verifies that state, then
publishes the seven receive buffers only after a release fence. RX DMA and its
interrupt remain disabled; the physical adapter is still pending.

`--prepare-owned-global-tx-rings` is the inactive physical adapter for this
preflight. It maps three separate low-32-bit pages filled entirely with
CPU-owned reset descriptors, applies and verifies all 18 ring slots plus the
global DTX reset while DMA and interrupts remain disabled, VFIO-resets while
all pages are still pinned, and only then unmaps them. It cannot enable DMA,
publish a producer index, install an IRQ, or send an MCU command.

The original physical attempt faulted because this operation's WFDMA BAR page
was accidentally mapped read-only before the first ring write. Page access is
now an explicit per-operation contract, and VFIO READ/WRITE/MMAP region flags
are checked before `mmap`. A guarded rerun wrote and read back all 18 owned
rings, reset every DTX index, reset the device while all three IOVAs remained
pinned, and then unmapped them. The root-only report is
`/var/lib/wifi-driver-lab/reports/20260802T165342Z-0000_05_00.0.log`.

`encode_download_command` ports the exact 64-byte legacy Connac2 command TXD
and request bodies for patch-semaphore acquisition, `PATCH_START`,
`TARGET_ADDRESS_LEN`, and `FW_START_REQ`. It rejects sequence zero/outside the four-bit firmware
range, an empty download, and a patch-start address other than MT7961's
`0x00900000`. Encoding these commands is not permission to send them: an owned
MCU TX ring, RX response ring, parsed matching response, VFIO IRQ, and safe
reset-while-pinned teardown must all exist first.
`parse_download_response` bounds the fixed 36-byte Connac2 MCU RX header and
matches the four-bit command sequence before exposing event identifiers; it is
the first pure parser needed by the future owned RX response ring.

`--query-patch-semaphore` is the first bounded active boot-ROM transaction.
It retains all mappings in one operation, replaces all 18 TX and all eight RX
ring slots with pinned userspace backing, and gives RX ring zero seven distinct
2 KiB response buffers. Before DMA it performs the pinned Linux conn-on
ownership and WFSYS reset sequences, installs an eventfd-backed MSI/MSI-X
vector while sources remain masked, disables L0s, acquires MT_TOP ownership,
and selects normal firmware mode. It then enables only RX0 completion, sends
`NIC_POWER_CTRL`, requires firmware-download state, and sends
`PATCH_SEM_CONTROL(GET)`. A response is accepted only from a completed RX
descriptor with a matching sequence and patch-semaphore event ID. Result 2 is
immediately followed by a matched semaphore release; result 1 needs no
release. Every exit masks both interrupt gates, disables and polls both DMA
directions, disables PCI bus mastering, resets through VFIO while all IOVAs
remain pinned, and only then unmaps. This operation does not scatter firmware
or claim N9/NIC capability readiness.

The guarded physical run completed this boot-ROM boundary through a VFIO MSI
vector. WFSYS became ready at 57 ms; the `NIC_POWER_CTRL` response (sequence 1,
event 3) was drained as unrelated; patch semaphore GET returned result 2 on
sequence 2; and the mandatory release returned result 3 on sequence 3. Each
response arrived through RX descriptors 0, 1, and 2 respectively with an
eventfd count, and the run disabled PCI bus mastering, reset while all mappings
remained pinned, and restored `mt7921e`, iwd, network reachability, and SSH.
N9 remained deliberately not ready. The root-only durable report is
`/var/lib/wifi-driver-lab/reports/20260802T174640Z-0000_05_00.0.log`.

`--inventory-vfio-irqs` queries the standard VFIO INTx, MSI, and MSI-X
capabilities without installing or triggering one, rejects modes without
eventfd support, and reports the preferred MSI-X/MSI/INTx choice. Device
interrupt unmasking remains unavailable until that chosen vector is actually
installed and exercised by the deterministic completion path.
`IrqLifecycle` prevents the device source from being enabled before an
eventfd-capable VFIO vector is installed, rejects a zero eventfd counter, and
requires explicit disable after an observed completion. The native backend now
has an unexposed `VfioIrq` owner which creates a nonblocking close-on-exec
eventfd, installs exactly one selected vector with `VFIO_DEVICE_SET_IRQS`,
drains 64-bit counters, explicitly disables the vector, and repeats disable in
`Drop`. Its Linux UAPI layout is tested, but it cannot yet be invoked physically
or unmask a device source.

`--install-disable-vfio-irq` exposes only the source-disabled lifecycle check:
select one eventfd-capable VFIO vector, install it, require its nonblocking
counter to remain empty while the device mask is zero, explicitly disable it,
and VFIO-reset. It never writes the device interrupt mask or enables DMA.

`teardown_pinned_dma` makes reset ordering explicit for the future active path:
mask and disable are attempted, TX busy is polled for at most 100 ms, and VFIO
function reset is issued while every IOVA remains pinned regardless of the poll
result. Mappings are released only after reset succeeds. A reset failure never
calls unmap, so the external reboot watchdog remains the containment boundary.
The native backend has an active-operation signal guard for
SIGHUP, SIGINT, and SIGTERM which performs only an atomic cancellation request
in the handler and restores previous handlers on drop. The future physical
control loop checks that request throughout command, scatter, and readiness
waits and enters the same reset-while-pinned containment path.

## Verified against pinned Linux 7.2-rc5 source

All paths below are relative to
`reference/linux-7.2-rc5/drivers/net/wireless/mediatek/mt76/`.

* `dma.h` defines `struct mt76_desc` and its length/last-section/done bits.
  `dma.c:mt76_dma_add_buf` fills up to two buffers per descriptor and
  `dma.c:mt76_dma_queue_reset` marks cleared descriptors DMA-done. The Rust
  `DmaDescriptor` ports only those word layouts. `mt7921/pci.c:mt7921_pci_probe`
  calls `dma_set_mask(..., DMA_BIT_MASK(32))`, so the Rust constructor rejects
  IOVAs above 32 bits rather than truncating them.
* `mt76_connac_mcu.h` defines packed `mt76_connac2_fw_trailer` (36 bytes) and
  `mt76_connac2_fw_region` (40 bytes). The region records precede the final
  trailer; their payloads are concatenated at the beginning of the image.
  `mt76_connac_mcu.c:mt76_connac_mcu_send_ram_firmware` walks those records,
  skips `FW_FEATURE_NON_DL`, initializes each download, sends `FW_SCATTER`, and
  starts firmware. `mt7921/mcu.c:mt7921_load_clc` walks the same layout to find
  a non-download `FW_TYPE_CLC` region. `Firmware` ports the common, bounded
  layout parsing only; it deliberately performs no MCU operation.
* PCI IDs `14c3:7961` and `14c3:7922` (plus the aliases in the table) select
  MT7921 and MT7922 firmware in `mt7921/pci.c:mt7921_pci_device_table`.
  Firmware names are `mediatek/WIFI_RAM_CODE_MT7961_1.bin`,
  `mediatek/WIFI_MT7961_patch_mcu_1_2_hdr.bin`,
  `mediatek/WIFI_RAM_CODE_MT7922_1.bin`, and
  `mediatek/WIFI_MT7922_patch_mcu_1_1_hdr.bin` in `mt792x.h`.

The Rust parser validates that the complete region table exists and that the
sum of payload lengths does not overlap metadata. This is memory-safe input
validation around the documented Linux layout, not a claim that every semantic
firmware constraint or CRC is understood. In particular, CRC validation and
the separate big-endian ROM-patch format remain unported.

## Responsibility boundary that did not port

The rest is not device-independent glue:

* **PCI and power:** `mt7921_pci_probe` enables the function and memory space,
  enables bus mastering, requests one IRQ vector of any PCI type, selects a
  32-bit DMA mask, maps BAR 0, optionally changes ASPM, negotiates firmware and
  driver ownership, reads chip revision, performs Wi-Fi-subsystem reset, and
  enables PCI MAC interrupts. `mt7921/pci_mcu.c:mt7921e_driver_own` and
  `mt792x_core.c:mt792xe_mcu_{drv,fw}_pmctrl` poll ownership registers.
  Suspend/resume in `mt7921/pci.c` coordinates MCU HIF suspend/deep sleep, DMA
  idle/disable, interrupt synchronization, ownership, and WPDMA reinit.
* **DMA and interrupts:** `mt7921/pci.c:mt7921_dma_init` allocates coherent
  descriptor rings plus mapped RX/TX buffers, programs base/count/CPU indices,
  and enables WFDMA. The configured sizes include 2048 data TX descriptors,
  256 MCU TX, 128 firmware-download TX, 1536 data RX, and device-dependent MCU
  RX rings. `mt792x_dma.c:mt792x_irq_handler` masks the device interrupt and
  schedules a tasklet; `mt792x_irq_tasklet` reads and acknowledges causes,
  disables individual causes, and schedules TX/RX NAPI polls. Poll completion
  re-enables a cause. Correctness depends on coherent visibility, ordering
  descriptor writes before producer-index MMIO, and draining rings before
  re-enabling interrupts.
* **Firmware and reset:** `mt792x_core.c:mt792x_load_firmware` restarts the MCU,
  waits for power, loads the ROM patch under a firmware semaphore, downloads
  RAM regions over the firmware DMA queue, starts firmware, and waits for N9
  ready. `mt7921/pci_mac.c:mt7921e_mac_reset` quiesces interrupts/work/NAPI,
  discards pending MCU state and tokens, resets WPDMA, reloads firmware and
  EEPROM state, reinitializes MAC state, and restarts the PHY. A generic PCI
  function reset is not equivalent to either WPDMA or Wi-Fi-subsystem reset.
* **SoftMAC:** this Linux driver is not an Ethernet FullMAC boundary.
  `mt7921/main.c:mt7921_ops` implements `ieee80211_ops` for interface/station
  lifecycle, keys, BSS changes, AMPDU, scans, channel contexts/switches, remain
  on channel, suspend/WoWLAN, SAR/regulatory handling, and TX queue wakeup.
  `mt76/mac80211.c`, `tx.c`, `agg-rx.c`, and `channel.c` supply shared mac80211
  station/WCID, TXQ, aggregation/reorder, channel, survey, and status behavior.
  skb ownership, NAPI, workqueues/tasklets/timers, cfg80211 regulatory state,
  and mac80211 callbacks must be replaced by an explicit portable SoftMAC
  service contract and scheduler; none belongs in this format crate.

This is an explicit **SoftMAC result**, despite substantial MCU and hardware
offload. The source exposes more than 50 `ieee80211_ops` callbacks, and TX still
arrives as mac80211 frames through `mt792x_tx`/`wake_tx_queue`, while RX is
reported through mt76/mac80211 status and reorder paths. Firmware commands
offload operations such as scanning, key/station programming, beaconing, and
aggregation setup; they do not expose the Ethernet-oriented FullMAC contract
used by the BCM4387 product direction. A userspace port would therefore need to
own at least: 802.11 frame TX/RX and status, authentication/association and
management exchange, station/BSS/key state, TXQ scheduling and rate/status
feedback, AMPDU reorder/BA lifecycle, scan/ROC/channel-context state machines,
regulatory/SAR/channel selection, power-save/WoWLAN, and timers/concurrency.
That is a portable SoftMAC stack plus the mt76 hardware transport, not merely a
replacement for Linux PCI/DMA calls. This makes MT7921/MT7922 useful here as a
broker/interface stress test, but a poor fit for the current FullMAC milestone.

## Concrete gaps in `wit/hardware.wit`

The present broker is enough to allocate an arena, copy bytes into it, obtain
an IOVA, access allowlisted BAR words, wait for a notification, and request an
opaque reset. It is not yet a suitable MT7921 PCI transport because it lacks:

1. a way to constrain DMA allocation to the device's 32-bit address mask;
2. explicit DMA publish/acquire or cache-maintenance operations and ordering
   relative to MMIO producer/consumer-index writes (required on non-coherent
   hosts; copied `read`/`write` alone does not state this contract);
3. firmware/artifact resources, so neither named RAM firmware nor ROM patch can
   be supplied to a no-WASI component;
4. PCI identity/revision/configuration capabilities for matching the variant,
   enabling memory decoding and bus mastering, selecting IRQ mode, controlling
   ASPM/power/wakeup, or querying reset support;
5. reset scopes that distinguish PCI function reset, WFSYS reset, WPDMA reset,
   and recovery which preserves/rebuilds DMA mappings;
6. an interrupt mask/ack/drain/re-enable contract. BAR operations can perform
   device masking and acknowledgement, but `interrupt.wait-until` does not say
   how a shared/level interrupt remains quiesced or how notifications interact
   with those MMIO writes;
7. cancellation/concurrent waiting suitable for integrating IRQ, MCU timeout,
   power, and SoftMAC timers into one portable event loop.

The broker also chooses allowlisted BAR regions, but the needed translated
register windows in `mt7921/pci.c:__mt7921_reg_addr` and
`mt7921_reg_map_l1` cannot be confirmed until the actual device and BAR policy
are inventoried.

## Verified no-plastic hardware boundary

`no-plastic` exposes MediaTek `14c3:7961` with subsystem `1a3b:4680` at
`0000:05:00.0`, normally bound to `mt7921e`. It is the sole member of IOMMU
group 19 and advertises function-level and bus reset methods. Bluetooth is not
in that PCI group. The native backend has attached its VFIO cdev to iommufd,
mapped and unmapped one private page at IOVA `0x0100_0000`, explicitly destroyed
the IOAS, and returned the function to `mt7921e`; it did not map BARs or arm an
interrupt. iwd reconnects after each handoff, although the kernel interface name
advances from `wlan0` to `wlanN` after reprobe.

RF-kill/wakeup wiring, ASPM quirks, and reset behavior beyond the repeatedly
successful VFIO-reset-and-native-rebind boundary remain unverified.

### Spike-only D0 handoff result

The Linux 6.18.40 lab kernel patch keeps one runtime-PM reference after the
native driver's DMA/IRQ teardown for exactly `14c3:7961` subsystem
`1a3b:4680`. With `mt7921e.keep_d0_on_remove=1`, watchdog-guarded report
`/var/lib/wifi-driver-lab/reports/20260810T094728Z-0000_05_00.0.log` reached
the durable stage `vfio_attached_d0_preflight_already_ready`: the VFIO cdev
opened, iommufd opened, the device bound to iommufd, an IOAS was allocated and
attached, and the function still reported D0 with memory decoding enabled and
bus mastering disabled. The host then wedged before a later durable stage,
with `VFIO_DEVICE_GET_REGION_INFO` next in the acquisition sequence. No BAR
mapping, firmware load, DMA publication, radio operation, or SAE MPDU is proven
by this run.

The reboot watchdog recovered into the same patched closure with `mt7921e`
bound in D0, iwd active, `wlan0` up, and the default route restored. The
on-disk stage file and forced syncs are deliberately spike-only crash-tracing
instrumentation, not a production logging contract.

A discovery-only follow-up from commit `2f0df96d` reached device info
`argsz=24 flags=0x3 num_regions=9 num_irqs=5` and completed region-info
queries for indices 0 through 7 without mapping any region. Index 0 reported
the 1 MiB read/write/mmap BAR, indices 2 and 4 reported 16 KiB and 4 KiB
read/write/mmap regions, and index 7 reported a 4 KiB read/write region.
Indices 1, 3, 5, and 6 reported zero-size/zero-flag regions. The exact last
durable marker was
`vfio_device_get_region_info_error index=8 argsz=32 error=query VFIO region:
Invalid argument (os error 22)`. IOAS destruction while the device remained
attached returned `EBUSY`; process close and the supervisor nevertheless
restored `mt7921e` with `RESTORE end failed=0`. Report
`/var/lib/wifi-driver-lab/reports/20260810T095455Z-0000_05_00.0.log` contains
the complete per-ioctl trace. Watchdog recovery again returned to the patched
kernel with the native adapter in D0, iwd active, and the default route healthy.
Index 8 is the fixed VFIO PCI VGA-region ABI slot, so `EINVAL` is the expected
absence result for this non-VGA function; useful discovery completed through
the PCI configuration region at index 7. The `EBUSY` result separately shows
that discovery cleanup must close or detach the VFIO device before destroying
its attached IOAS.

The corrected discovery-only run uses `VFIO_DEVICE_DETACH_IOMMUFD_PT` before
IOAS destruction and treats `EINVAL` only on non-required region slots as an
absent region. Report
`/var/lib/wifi-driver-lab/reports/20260810T100543Z-0000_05_00.0.log` records
index 8 as absent, then reaches both `vfio_region_discovery_complete` and
`vfio_region_discovery_released_safe`. Userspace exited zero and supervisor
restoration ended with `failed=0`; there was no BAR mapping and no watchdog
reboot. The boot ID remained `87e137b8-3d23-493d-af4c-4c1ff447876a`, and the
native driver returned in D0 with iwd and the default route active.

The next guarded boundary mapped only BAR0 page zero for read access and
immediately unmapped it without dereferencing the mapping. Report
`/var/lib/wifi-driver-lab/reports/20260810T100928Z-0000_05_00.0.log` durably
records `vfio_bar0_mmap_before`, `vfio_bar0_mmap_after`,
`vfio_bar0_munmap_before`, and `vfio_bar0_munmap_after`, followed by
`vfio_region_discovery_released_safe`. No MMIO read or write, firmware action,
or DMA mapping occurred. Userspace returned zero, restoration ended with
`failed=0`, and the watchdog disarmed automatically after its absolute-path
health checks observed the native driver, iwd, and the default route. The boot
ID remained `87e137b8-3d23-493d-af4c-4c1ff447876a`.

Pinned Linux `mt792x_regs.h` defines `MT_INFRA_CFG_BASE` as direct BAR offset
`0xfe000` and `MT_HIF_REMAP_L1` as `MT_INFRA(0x24c)`, yielding direct BAR0
offset `0xfe24c`. Pinned `mt7921_reg_map_l1` passes that register to
`mt76_rmw_field` and then reads it with `mt76_rr` to push the selector write;
the existing `VfioDynamicL1::read_selector` likewise reads exactly
`MT_HIF_REMAP_L1_BAR_OFFSET` before any selector update. This establishes the
selector itself as a directly addressed readable register, unlike the
identity targets behind its indirect window.

The guarded read-only follow-up mapped only BAR0 page `0xfe000`, executed one
volatile 32-bit read at `0xfe24c`, and observed `0x18451800`. Report
`/var/lib/wifi-driver-lab/reports/20260810T101318Z-0000_05_00.0.log` contains
the durable before/after read markers and the subsequent munmap and safe
release markers. It performed no selector write, indirect-window read, other
MMIO access, firmware action, DMA, or radio operation. Userspace and restore
both returned success, the watchdog disarmed automatically, and the unchanged
boot returned the native driver in D0 with iwd and the default route healthy.

The minimal write boundary did not hardcode that observation: it mapped only
BAR0 page `0xfe000` read/write, read and retained the selector, wrote that exact
runtime value back once, and read it once more. Report
`/var/lib/wifi-driver-lab/reports/20260810T101540Z-0000_05_00.0.log` records
saved value `0x18451800`, the single identity write of `0x18451800`, and equal
readback `0x18451800`, followed by munmap and safe release. No selector bits
changed and there was no indirect-window access, other MMIO, firmware, DMA, or
radio operation. Userspace returned zero and supervisor restoration reported
`failed=0`, but the native network did not become remotely reachable before
the watchdog deadline. Recovery therefore rebooted to
`cd298031-f2f5-4911-8c90-8d9e89bb40b8`, where the patched kernel, native driver
in D0, iwd, and the default route were healthy. Thus the identical-value write
is mechanically verified but, unlike the read-only boundary, does not yet
prove reboot-free native-network recovery.

Postmortem evidence from that boot is limited because the effective journald
configuration ends with `Storage=volatile` and `RuntimeMaxUse=16M`; boot ID
`87e137b8-3d23-493d-af4c-4c1ff447876a` has no entries after reboot, including
no retained kernel, iwd, or watchdog messages. NetworkManager, networkd, and
dhcpcd are not installed; iwd owns association and network configuration. The
durable report was created at `2026-08-10 15:45:40 IST` and last written at
`15:45:42.025815 IST` after `RESTORE end failed=0`. That restore result proves
the PCI device reprobed as `mt7921e`, its override cleared, udev settled, iwd
started active, and the state file was removed. The local health supervisor
then checked those same facts plus a default route every two seconds for 60
seconds; because it did not disarm, the missing fact was the route, not a
reported reprobe or iwd service-start failure. The recovery kernel began at
`15:47:50.972681 IST`, about 129 seconds after restore completed, consistent
with expiry of the 120-second watchdog lease and reboot startup. Whether a new
`wlanN` appeared but association/DHCP lagged or association failed cannot be
recovered from the volatile journal.

Before repeating the identical-value write, the supervisor should fsync a
two-second post-restore timeline to `/var/lib/wifi-driver-lab`: driver link and
PCI power state, every `wlan*` name/operstate/address, iwd state, default route,
and kernel/iwd journal excerpts. That is the smallest rerun able to distinguish
interface rename, firmware/reprobe failure, association failure, and route
latency without changing the selector value.
