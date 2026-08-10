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
`PCIE_LPCR_HOST_OWN_SYNC` bit. It preserves Linux's ten independently timed
50 ms attempts and 1 ms poll tick; like Linux, poll success masks only
`OWN_SYNC` and ignores echoed command bits. Every write, status sample, retry,
terminal success, or timeout is emitted as a structured event. No firmware-ownership or
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

That instrumented identical-value rerun completed without a failed recovery
transition. Report
`/var/lib/wifi-driver-lab/reports/20260810T102304Z-0000_05_00.0.log` again
records saved selector `0x18451800`, one write of the same runtime value, equal
readback, and safe release. The fsynced timeline is
`/var/lib/wifi-driver-lab/selector-write-recovery-20260810T102304Z.log`, with
the bounded kernel/iwd window in the adjacent `.messages.log`.

Restore returned zero at `15:53:05.966697 IST`. By then mt7921e had logged
ASIC revision `79610010` at `15:53:05.856833`, firmware versions by
`15:53:05.941854`, and iwd started at `15:53:05.957059`; there was no reprobe
or firmware-init error. iwd first announced `wlan0` at `15:53:06.798926`, then
the usable interface `wlan1` at `15:53:07.352426`. The timeline therefore saw
no WLAN at sample 0, disconnected/scanning `wlan1` at `15:53:08.010741`, and
connecting `wlan1` without an address at `15:53:10.059721`. The AP rejected
the first authentication attempt with status 77 at `15:53:10.062851`, but the
immediate retry authenticated and associated by `15:53:10.147843`. iwd entered
netconfig at `15:53:11.217967` and connected at `15:53:11.284564`.

At `15:53:12.117522`, 6.15 seconds after restore returned, the timeline
separately recorded association, IPv4 `192.168.235.6/24`, the default route on
`wlan1`, and successful gateway reachability. The all-interface check was not
fooled by the rename. The watchdog disarmed at `15:53:12.180541`, and boot ID
`cd298031-f2f5-4911-8c90-8d9e89bb40b8` remained unchanged. Thus no required
transition failed in this rerun; the only error was the recovered initial
status-77 authentication response. The earlier watchdog recovery remains an
unreproduced association/netconfig failure rather than evidence of failed
mt7921e reprobe or a deterministic selector identity-write side effect.

The first changed-selector boundary then used only the pinned
`mt7921_reg_map_l1` sequence needed for the two identity words. Report
`/var/lib/wifi-driver-lab/reports/20260810T103027Z-0000_05_00.0.log` records
the runtime selector `0x18451800`, one selection write to `0x18457001` for L1
base `0x7001`, and posted-write verification before any indirect read. The
read-only window returned `MT_HW_CHIPID = 0x00007961` from physical
`0x70010200` and `MT_HW_REV = 0x00008a10` from `0x70010204`. The run then
wrote back the exact saved selector, verified full equality with
`0x18451800`, unmapped both pages, and reached safe VFIO release. It performed
no other window read, firmware action, DMA mapping, or radio operation.

Userspace and restoration both returned zero without changing boot ID
`cd298031-f2f5-4911-8c90-8d9e89bb40b8`. The fsynced recovery timeline is
`/var/lib/wifi-driver-lab/selector-write-recovery-20260810T103027Z.log`, with
the bounded kernel/iwd window in the adjacent `.messages.log`. Restore returned
at `16:00:29.159243 IST`; native mt7921e had logged ASIC revision `79610010`
at `16:00:29.043032`, firmware identity by `16:00:29.129854`, and iwd started
at `16:00:29.149974`. iwd announced the usable `wlan3` at
`16:00:30.522528` and reached connected state at `16:00:34.825973`. At
`16:00:35.296528`, 6.14 seconds after restore returned, the supervisor
observed association, IPv4 `192.168.235.6/24`, the default route, and a
successful gateway ping together; it disarmed the watchdog at
`16:00:35.356926`. The native driver remained bound in D0 and iwd remained
active.

### Next boundary after dynamic identity

The temporary `--run-one-shot-sae-auth` preflight currently stops and releases
VFIO immediately after the proven `MT_HW_CHIPID` and `MT_HW_REV` reads. It must
not be advanced by merely deleting that return: the continuation acquires the
full active-MCU resource set and eventually reaches interrupt, reset, WFDMA,
firmware, and radio operations.

At pinned Linux commit `e8efe09d4f378992c890d181d65e2ed8d8cb1194`,
`mt7921/pci.c:mt7921_pci_probe` reads `MT_HW_CHIPID`, conditionally reads
`MT_HW_BOUND`, then reads `MT_HW_REV`. `mt792x_regs.h` defines
`MT_HW_BOUND = 0x70010020`; under the already proven L1 base `0x7001` this is
one aligned volatile 32-bit read at BAR0 offset `0x40020`. Linux tests only bit
7 when the chip ID is `0x7961`: set changes the effective chip ID to `0x7920`,
clear retains `0x7961`. It does not write this register and does not interpret
the other bits. The low eight bits of the subsequent revision word form the
low byte of Linux's composite ASIC revision.

Pinned Fuchsia commit `1e1219e3fac944c9a906aea9646939746b6062b3` has no PCI,
L1-remap, or MT7921 identity operation at this point. Its client MLME begins at
the `DeviceOps`/SoftMAC contract after hardware initialization, so this read is
Linux-derived transport mechanics rather than Fuchsia policy.

The smallest independently reversible next boundary is therefore to extend
the existing narrow preflight with exactly the `MT_HW_BOUND` read, not to enter
the full continuation. It must retain the read-only `0x40000` window mapping,
derive selector `0x7001` from the complete saved selector, verify the posted
selector write, reject a chip ID other than `0x7961`, perform no window access
other than the three closed identity offsets, restore and verify the exact
saved selector on every exit, then unmap and release. An all-ones read must
fail closed as invalid MMIO evidence. No PCI command, interrupt gate, WFSYS,
WFDMA, firmware, DMA, or radio operation belongs in this boundary.

That boundary completed in report
`/var/lib/wifi-driver-lab/reports/20260810T104105Z-0000_05_00.0.log` using
release binary SHA-256
`56481a2b11a98be13535db50e27027eeaec113b884350172dce28c3eb928386a`.
Under one saved selector `0x18451800`, the run selected and verified
`0x18457001`, then read in pinned Linux order: `MT_HW_CHIPID = 0x00007961`,
`MT_HW_BOUND = 0x00000018`, and `MT_HW_REV = 0x00008a10`. Bound bit 7 is clear,
so Linux retains effective chip ID `0x7961`; combining it with revision low
byte `0x10` yields composite revision `0x79610010`, matching the native
driver's ASIC log. The run restored and verified exact selector equality with
`0x18451800`, unmapped both pages, reached safe VFIO release, and returned zero.
The temporary early return remained in place, so no PCI command, interrupt,
reset/WFSYS/WFDMA, firmware, DMA, or radio operation followed.

The recovery timeline is
`/var/lib/wifi-driver-lab/selector-write-recovery-20260810T104105Z.log`, with
the bounded kernel/iwd window in the adjacent `.messages.log`. Restore returned
at `16:11:07.612080 IST`; mt7921e was already bound in D0 and iwd active. Native
firmware identity completed by `16:11:07.578832`, iwd announced usable `wlan4`
at `16:11:08.984206`, and reached connected state at `16:11:12.918349`.
Association, IPv4 `192.168.235.6/24`, default route, and gateway ping were all
observed at `16:11:13.750197`, 6.14 seconds after restore. The watchdog
disarmed at `16:11:13.812929`; boot ID
`cd298031-f2f5-4911-8c90-8d9e89bb40b8` remained unchanged.

Verification remains deliberately scoped. The release workspace passed all 69
`mt7921-passive-scan` tests and a locked release build, with existing upstream
unused-code/import warnings. The root/default-feature `mt7921-port-spike` test
command remains blocked by pre-existing unrelated errors: firmware-inspect
calls `world_clc_commands` without its fourth argument, and non-
`fuchsia-passive` compilation references the cfg-gated SAE operation/stage
logger. A rustfmt check of `vfio_read.rs` likewise still reports pre-existing
formatting drift around the device-info marker and discovery-release marker;
those unrelated lines were not changed.

### Next active-acquisition boundary after identity

The exact continuation behind the temporary early return first re-queries BAR0
region metadata and maps BAR pages `0xd4000`, `0x10000`, and `0xe0000`; mapping
alone does not dereference a register or change device state. The first
device-visible operation in `active_preflight` is then
`verify_pci_dma_disabled`: a read-only 256-byte PCI configuration snapshot. It
reads the 16-bit Command register at configuration offset `0x04`, requires
Memory Space Enable (bit 1) set and Bus Master Enable (bit 2) clear, walks the
standard capability list from offset `0x34`, and requires the Power Management
Control/Status Register power-state field to report D0 (`00b`). IRQ-capability
and VFIO reset-capability queries follow, but are not part of this boundary.
DMA mappings occur only after those preconditions.

The first later state mutation is `disable_pci_intx`, which reads the same
Command word and sets Interrupt Disable bit 10 (`0x0400`). This is an adapted
userspace form of pinned Linux
`drivers/pci/pci.c:pci_intx(pdev, 0)` at commit
`e8efe09d4f378992c890d181d65e2ed8d8cb1194`; pinned
`include/uapi/linux/pci_regs.h` defines Command offset `0x04`, Memory Space
Enable `0x0002`, Bus Master Enable `0x0004`, and INTx Disable `0x0400`.
Requiring BME clear during handoff is stricter lab containment: normal pinned
`mt7921_pci_probe` enables memory decoding and bus mastering during native
probe. Pinned Fuchsia commit `1e1219e3fac944c9a906aea9646939746b6062b3`
does not own PCI Command or PMCSR; its SoftMAC `DeviceOps` boundary begins
after transport initialization.

The smallest next physical boundary is therefore read-only: while retaining
the temporary early return, take and durably record the complete post-identity
PCI Command word plus the located PM capability offset and raw PMCSR, verify
MSE=1, BME=0, and D0, then release exactly as the identity boundary does. It
needs no restoration because it performs no write, and it must stop before
`VFIO_DEVICE_GET_IRQ_INFO`, active-resource allocation, DMA mapping, interrupt
installation, reset, or any BAR dereference. This also proves that the selector
transaction did not perturb PCI command/power state.

If the subsequent INTx-disable write is later admitted, it must be a separate
boundary: save the entire 16-bit Command word; compute only
`selected = saved | 0x0400`; write exactly those two bytes at offset `0x04`;
read back and require full equality with `selected`; restore the exact saved
word on every exit; and read back full equality before release. If bit 10 was
already set, that boundary is an identical-value write and still requires the
same durable write/readback/restore evidence. It must not touch the PCIe MAC
interrupt gate at BAR0 `0x10188` in the same run.

The existing verification failures do not change these hardware semantics.
The locked `mt7921-passive-scan` build used for physical gates enables
`fuchsia-passive` by default and compiles this path. The standalone
`mt7921-port-spike` default-feature failure is configuration-specific: the same
source file references cfg-gated SAE names without that feature; the separate
firmware-inspect missing argument is unrelated. Rustfmt drift is also
non-semantic, although one reported hunk is nearby in the device-info marker
and the other is in the unreachable discovery-release continuation. A future
implementation should format only its changed lines or separately fix that
pre-existing drift; neither issue authorizes weakening the boundary.

The read-only post-identity PCI gate completed in report
`/var/lib/wifi-driver-lab/reports/20260810T104636Z-0000_05_00.0.log` using
release binary SHA-256
`fb1c1c19ed3de71e8b15fe91246437172865695256d4c9098f259e8355427325`.
After the unchanged identity sequence restored and verified selector
`0x18451800`, one 256-byte configuration read returned full PCI Command
`0x0002`, PM capability offset `0xf8`, and raw PMCSR `0x0008`. Thus MSE was set,
BME was clear, and PMCSR power-state bits were zero (D0). INTx Disable bit 10
was also clear; a future disable boundary would change Command from `0x0002`
to `0x0402`, not perform an identical-value write. The gate then unmapped only
the two existing identity pages and reached safe release. It did not map the
full continuation's WFDMA, PCIe-MAC, or CONN pages and performed no PCI write,
BAR dereference beyond the proven identity closure, IRQ/reset query, DMA
mapping, firmware, WFDMA, or radio operation.

The recovery timeline is
`/var/lib/wifi-driver-lab/selector-write-recovery-20260810T104636Z.log`, with
the bounded kernel/iwd window in the adjacent `.messages.log`. Restore returned
at `16:16:38.365253 IST`; sample zero already observed native mt7921e in D0 and
iwd active. iwd announced usable `wlan5` at `16:16:39.743045` and reached
connected state at `16:16:43.992928`. The AP briefly disassociated the first
association with reason 2, then the retry succeeded. Association, IPv4
`192.168.235.6/24`, default route, and gateway ping were all observed at
`16:16:44.509745`, 6.14 seconds after restore, and the watchdog disarmed at
`16:16:44.570324`. Boot ID `cd298031-f2f5-4911-8c90-8d9e89bb40b8` remained
unchanged. The same verification limits apply: all 69 locked release-workspace
tests and the locked release build passed with existing upstream warnings;
the unrelated standalone default-feature and rustfmt failures remain as
recorded above.

The isolated INTx-disable round trip completed in report
`/var/lib/wifi-driver-lab/reports/20260810T104952Z-0000_05_00.0.log` using
release binary SHA-256
`342f4692a9681349ea3485e2cbd998b6d616cb03e5c97322776f0c36b90b8bb5`.
After the unchanged identity and PCI-preflight gates, the transaction saved
full Command `0x0002`, wrote exactly two bytes at configuration offset `0x04`
for selected value `0x0402`, and verified full readback equality. It then wrote
the exact saved two bytes on the unconditional restore path and verified full
equality with `0x0002` before publishing completion. The existing identity
pages were then unmapped and VFIO released safely. No other PCI field, BAR
mapping or dereference, VFIO IRQ/reset query, DMA mapping, firmware, WFDMA, or
radio operation was admitted.

The recovery timeline is
`/var/lib/wifi-driver-lab/selector-write-recovery-20260810T104952Z.log`, with
the bounded kernel/iwd window in the adjacent `.messages.log`. Restore returned
at `16:19:53.966904 IST`; sample zero already saw mt7921e in D0 and iwd active.
iwd announced usable `wlan6` at `16:19:55.332584`, authenticated and associated
by `16:19:58.129848`, and reached connected state at `16:19:59.279892`.
Association, IPv4 `192.168.235.6/24`, default route, and gateway ping were all
observed at `16:20:00.108705`, 6.14 seconds after restore, and the watchdog
disarmed at `16:20:00.171036`. Boot ID
`cd298031-f2f5-4911-8c90-8d9e89bb40b8` remained unchanged. All 69 locked
release-workspace tests and the locked release build passed with the existing
upstream warnings; the previously recorded standalone default-feature and
rustfmt limitations remain unchanged.

### VFIO query boundary under temporary INTx disable

The real active path has an important ordering distinction. Its
`active_preflight` queries IRQ and reset capabilities before allocating active
resources and before persistently disabling INTx. After later
`disable_pci_intx`, the literal next device operation is not a query: it is a
volatile zero write to `MT_PCIE_MAC_INT_ENABLE` at BAR0 `0x10188`. That write
currently has no local saved-state rollback and must remain outside the next
gate. Repeating the already-required query preflight while Command is
temporarily `0x0402` is the smallest way to advance evidence without reaching
that BAR write.

At pinned Linux UAPI commit `e8efe09d4f378992c890d181d65e2ed8d8cb1194`,
`VFIO_DEVICE_GET_IRQ_INFO` is ioctl number `VFIO_BASE + 9`. The caller supplies
the 16-byte `vfio_irq_info` with `argsz` and `index`; the kernel returns
`flags` and `count`. PCI indices 0, 1, and 2 are respectively INTx, MSI, and
MSI-X. A zero count denotes an unimplemented type. Flag bit 0
`VFIO_IRQ_INFO_EVENTFD` says that index supports eventfd signaling; bits 1, 2,
and 3 report maskable, automasked, and no-resize behavior. This query does not
install an eventfd, select an IRQ mode, mask/unmask a source, or alter device
interrupt state. Those effects require the distinct `VFIO_DEVICE_SET_IRQS`
ioctl, which is excluded.

`VFIO_DEVICE_GET_INFO` is ioctl number `VFIO_BASE + 7`. The caller supplies
`vfio_device_info.argsz`; the kernel returns `flags`, `num_regions`,
`num_irqs`, and `cap_offset`. Flag bit 0 `VFIO_DEVICE_FLAGS_RESET` only
advertises that the device supports reset; the query does not reset it. Reset
requires the distinct `VFIO_DEVICE_RESET` ioctl (`VFIO_BASE + 11`), which is
excluded. Both GET ioctls are therefore read-only capability queries against
the already-open VFIO device fd. The current cdev path has already bound the
device to iommufd and attached its empty IOAS before identity; no IRQ install,
BAR mapping, or DMA mapping is an ioctl prerequisite. Existing code maps BAR
pages before active preflight only because the broader resource owner batches
later operations, not because either query uses them.

The smallest next boundary retains the early return and the two identity pages
only. After preflight saves Command `0x0002`, it writes and fully verifies
temporary `0x0402`; while that value is selected, it queries IRQ indices 0, 1,
and 2 in order and durably records each complete `argsz`, `flags`, and `count`.
It then selects MSI-X over MSI over INTx only when count is nonzero and EVENTFD
is set, fails closed if no such source exists or if only INTx wins, queries
complete device info, and requires RESET plus PCI flags without invoking reset.
The existing unconditional Command restore must run after query success or
failure and fully verify exact `0x0002` before unmapping and release. The gate
must stop before `VFIO_DEVICE_SET_IRQS`, `VFIO_DEVICE_RESET`, DMA mapping, BAR
`0x10188`, firmware, WFDMA, or radio. Pinned Fuchsia owns none of these VFIO or
PCI mechanics; its SoftMAC boundary remains downstream of transport setup.

That query-only gate completed in report
`/var/lib/wifi-driver-lab/reports/20260810T105414Z-0000_05_00.0.log` using
release binary SHA-256
`db0aa62d3fc70658a55cebf73b82fd2ea989e229e1b94f136988619168e0fe3a`.
While Command `0x0402` was fully verified, `VFIO_DEVICE_GET_IRQ_INFO` returned:
INTx index 0, `argsz=16`, flags `0x00000007`, count 1; MSI index 1,
`argsz=16`, flags `0x00000009`, count 32; and MSI-X index 2, `argsz=16`,
flags `0x00000009`, count 0. Thus INTx reports EVENTFD, MASKABLE, and
AUTOMASKED; MSI reports EVENTFD and NORESIZE; MSI-X reports the same flags but
is unimplemented because its count is zero. The pure preference selected MSI
with 32 vectors and did not install it.

The subsequent read-only `VFIO_DEVICE_GET_INFO` returned `argsz=24`, flags
`0x00000003`, nine regions, five IRQ indices, and capability offset zero.
RESET and PCI flags were therefore both present; no reset ioctl followed. The
unconditional rollback then wrote exact saved Command `0x0002` and verified
full equality before safe unmap/release. Only the two identity pages were
mapped. No `SET_IRQS`, `DEVICE_RESET`, BAR `0x10188`, DMA mapping, firmware,
WFDMA, or radio operation occurred.

The recovery timeline is
`/var/lib/wifi-driver-lab/selector-write-recovery-20260810T105413Z.log`, with
the bounded kernel/iwd window in the adjacent `.messages.log`. Restore returned
at `16:24:15.766081 IST`; sample zero saw mt7921e in D0 and iwd active. iwd
announced usable `wlan7` at `16:24:17.135169`, associated by
`16:24:17.428829`, and reached connected state at `16:24:18.559458`.
Association was visible at `16:24:17.809902`; IPv4 `192.168.235.6/24`, default
route, and gateway ping followed at `16:24:19.857688`, 4.09 seconds after
restore. The watchdog disarmed at `16:24:19.917976`, and boot ID
`cd298031-f2f5-4911-8c90-8d9e89bb40b8` remained unchanged. All 69 locked
release-workspace tests and the locked release build passed with existing
upstream warnings; previously recorded standalone default-feature and rustfmt
limitations remain unchanged.

### PCIe MAC interrupt-gate boundary

After persistent PCI INTx disable, the literal next active-path device access
is `write_pcie_mac_interrupt_enable_zero`, a volatile 32-bit zero write at BAR0
`0x10188`. Pinned Linux commit
`e8efe09d4f378992c890d181d65e2ed8d8cb1194` defines
`MT_PCIE_MAC_BASE = 0x10000` and
`MT_PCIE_MAC_INT_ENABLE = MT_PCIE_MAC(0x188)` in `mt792x_regs.h`, producing
`0x10188`. `mt7921/pci.c:__mt7921_reg_addr` returns every address below
`0x100000` unchanged, so this is a direct BAR0 offset, not an L1-remapped
register. The same function's fixed map also translates silicon address
`0x74030188` through PCIE_MAC_IREG base `0x74030000` to BAR0 `0x10188`; that
alternate expression still requires no selector transaction.

Linux accesses the register through `mt76_wr`, hence one aligned volatile
32-bit little-endian MMIO store. Probe/resume and WPDMA reinitialization write
`0x000000ff`; suspend, MAC reset, and WPDMA reinitialization write
`0x00000000`. These paired enable/disable values establish interrupt-enable
latch semantics rather than status acknowledgement or W1C command semantics.
Pinned source defines no individual bit names and never reads the register, so
it does not establish that immediate readback is architecturally required for
success or that bits outside the low byte are reserved. Existing lab code has
successfully used ordinary volatile reads for precondition and containment
checks, but the native value at the new D0 handoff boundary has not been
durably recorded.

Native teardown does not justify hard-coding zero or `0xff`.
`mt7921e_unregister_device` unregisters mt76, disables NAPI, takes driver
ownership, cleans DMA, and resets WFSYS, but has no local
`MT_PCIE_MAC_INT_ENABLE` write. Deeper generic teardown or hardware reset may
affect the latch, so its exact post-remove value is an observation, not an
invariant. The only safe initial checks are that the read succeeds and is not
all ones; interpretation should preserve the complete raw word.

The smallest next boundary is therefore read-only. Retain the current identity,
PCI, INTx, and VFIO-query sequence with temporary Command `0x0402`; map only
one additional BAR0 page, page `0x10000`, with read permission; perform exactly
one volatile 32-bit read at `0x10188`; durably record the raw value; reject
`0xffffffff`; unmap that page; then run the already-unconditional exact Command
restore/readback and early release. No other active-path BAR pages (`0xd4000`,
`0xe0000`, `0x9f000`, or `0xd6000`) are needed. The page is required only
because VFIO BAR MMIO is exposed by `mmap`; it is not required by the preceding
capability ioctls.

A later mutation must remain a separate gate. Save the exact 32-bit snapshot,
write only `0x00000000`, and use one ordinary volatile read to verify full zero
while treating that read as lab rollback evidence rather than a pinned-Linux
success condition. On every exit, write back the complete saved word and read
back full equality before unmapping; do not synthesize `0xff` or discard
unknown high bits. Command must likewise restore exactly from `0x0402` to
`0x0002`. That gate must still stop before `SET_IRQS`, reset, WFDMA/DMA,
firmware, or radio. Pinned Fuchsia does not own this PCIe interrupt latch; it
remains Linux-derived transport mechanics below SoftMAC.

The read-only latch snapshot completed in report
`/var/lib/wifi-driver-lab/reports/20260810T105906Z-0000_05_00.0.log` using
release binary SHA-256
`3a65924bf318b01a02cd218a5b7d112b04b4c1082841f4ea4268fc012219b61d`.
After temporary Command `0x0402` and the query-only capability checks, the gate
mapped BAR0 page `0x10000` read-only and performed exactly one aligned volatile
32-bit read at `0x10188`. Native handoff state was
`MT_PCIE_MAC_INT_ENABLE = 0x000000ff`. The gate explicitly unmapped page
`0x10000`, then restored and verified exact PCI Command `0x0002`, unmapped the
two identity pages, and released safely. It performed no register write,
`SET_IRQS`, reset, DMA mapping, firmware, WFDMA, or radio operation.

The recovery timeline is
`/var/lib/wifi-driver-lab/selector-write-recovery-20260810T105906Z.log`, with
the bounded kernel/iwd window in the adjacent `.messages.log`. Restore returned
at `16:29:08.841005 IST`; sample zero saw mt7921e in D0 and iwd active. iwd
announced usable `wlan8` at `16:29:10.208004`. The first association was
briefly disassociated with reason 2, then the retry associated at
`16:29:11.859865` and iwd reached connected state at `16:29:11.990117`.
Association, IPv4 `192.168.235.6/24`, default route, and gateway ping were all
observed at `16:29:12.937781`, 4.10 seconds after restore; the watchdog
disarmed at `16:29:13.002728`. Boot ID
`cd298031-f2f5-4911-8c90-8d9e89bb40b8` remained unchanged. All 69 locked
release-workspace tests and the locked release build passed with existing
upstream warnings; the standalone default-feature and rustfmt limitations
remain unchanged.

The isolated interrupt-latch mutation completed in report
`/var/lib/wifi-driver-lab/reports/20260810T110510Z-0000_05_00.0.log` using
release binary SHA-256
`d3b3a68fde35f7cdcc1b0bcc5e1c68d38cc255c07f0982b39e16abd660bcf32a`.
Under temporary Command `0x0402`, the gate mapped only BAR0 page `0x10000`
read-write, saved the complete runtime latch `0x000000ff`, wrote exactly one
full-word zero, and obtained the single full-zero readback. It then wrote back
the exact saved `0x000000ff`, read full equality, durably marked and explicitly
unmapped the page, restored and verified exact Command `0x0002`, unmapped the
two identity pages, and released safely. No `SET_IRQS`, reset, DMA mapping,
firmware, WFDMA, or radio operation occurred.

The recovery timeline is
`/var/lib/wifi-driver-lab/selector-write-recovery-20260810T110510Z.log`, with
the bounded kernel/iwd window in the adjacent `.messages.log`. Restore returned
at `16:35:12.646038 IST`; sample zero saw mt7921e in D0 and iwd active. This
run's network recovery was unusually slow but remained inside the independent
watchdog: association appeared on `wlan9` at `16:36:53.036274`, and IPv4
`192.168.235.6/24`, the default route, and gateway connectivity followed at
`16:36:55.086799`, 102.44 seconds after restore. The watchdog disarmed at
`16:36:55.148763`; boot ID `cd298031-f2f5-4911-8c90-8d9e89bb40b8` remained
unchanged. Final health was mt7921e in D0, iwd active, `wlan9` associated and
routed, and the gateway reachable. All 69 locked release-workspace tests and
the locked release build passed with existing upstream warnings; the
standalone default-feature and rustfmt limitations remain unchanged.

#### Slow-recovery postmortem

Read-only correlation of the durable timeline, its kernel/iwd message capture,
and the current boot journal shows that the 102.44-second recovery was not a
100-second PCI reprobe or firmware-load stall. VFIO reset ran from
`16:35:12.175833` to `16:35:12.279883`; mt7921e logged the ASIC at
`16:35:12.530882`, HW/SW firmware at `16:35:12.604854`, and WM firmware at
`16:35:12.615831`. iwd restarted at `16:35:12.636789`, discovered `phy9` at
`16:35:13.437626`, observed an initial `wlan0` with ifindex 22 at
`16:35:13.465911`, and observed the final `wlan9` station with ifindex 23 at
`16:35:14.016900`. The timeline's sample 1 saw `wlan9` scanning and
disconnected at `16:35:14.689386`. Thus an enumerated station interface existed
about 1.37 seconds after restore; enumeration/recreation and the final name did
not account for the long outage.

iwd selected `ph1` at `16:35:31.145126`. The AP first returned authentication
status 77, after which authentication succeeded at `16:35:31.235827`; all
eight earlier captured recoveries also contain the same status-77 exchange and
then recovered quickly, so that exchange is not unique to this incident. The
kernel sent three association requests but received no association response,
timed out at `16:35:31.551933`, and iwd recorded `association-timeout`,
`CMD_ASSOCIATE (-2)`, and `connect-failed` before returning to
`autoconnect_full`. iwd did not select the AP again until
`16:36:52.017412`, an 80.44-second retry interval. That second attempt received
a successful association response at `16:36:52.079221`, associated at
`16:36:52.089881`, and reached iwd `connected` at `16:36:53.209008`. The
proximate cause of the long recovery was therefore one missing association
response followed by iwd's retry/backoff interval. The logs cannot determine
whether that missing response was an AP, RF, driver receive, or protocol
transient, and do not justify attributing it to the restored interrupt latch.

There was no rfkill event; the radio is currently neither soft nor hard
blocked. Every sampler iteration reported mt7921e attached, PCI D0, and runtime
active, with no PCI/AER or firmware error. The `page_pool_release_retry`
warning at `16:36:12.488911` concerns a retiring pool from the removed
interface, not delayed reprobe: similar warnings occurred after earlier fast
recoveries, and this run's new interface was already scanning before the
warning and later associated without another reprobe. It remains a teardown
diagnostic worth retaining, but is not evidence for the 80-second wait.

Sample 0 at `16:35:12.656384` was diagnostically premature: its fields only
proved the driver symlink, D0/runtime-active PCI state, and an active iwd
process. It printed no WLAN interface, and iwd did not discover the wiphy for
another 0.78 seconds. This does not weaken the watchdog completion guard,
which also required carrier, IPv4, default route, and gateway connectivity,
but sample 0 must be described only as lower-layer/service presence rather
than usable Wi-Fi health.

No further mutation gate should use this recovery as a routine baseline. The
smallest next diagnostic is one supervised recovery-only control using the
same detach/VFIO/restore path with a payload that performs no device access,
while retaining the current journal capture. Before that control, the
procedural progression guard should classify any association timeout,
`connect-failed`, or restore-to-connectivity interval over 60 seconds as an
anomalous recovery that blocks the next gate even if the 120-second watchdog
eventually disarms. The sampler should also record explicit wiphy/interface
readiness separately from its PCI/iwd-process fields. Do not extend the
watchdog merely to make this run appear routine; first determine whether the
failure repeats in the zero-access control.

The recovery-only control completed under the unchanged 120-second watchdog
in report
`/var/lib/wifi-driver-lab/reports/20260810T111422Z-0000_05_00.0.log` and
timeline
`/var/lib/wifi-driver-lab/selector-write-recovery-20260810T111422Z.log`.
Supervisor SHA-256 was
`7149772f9be7d708150581f759babdfb176486d97202c368a95439d376d49030`.
The native lab wrapper performed the same detach, VFIO acquisition and
release, and native restore, but its sole userspace payload was
`/run/current-system/sw/bin/true`; the report contains only payload begin/end
with return code zero. The payload opened no VFIO device and performed no PCI
configuration access, BAR mapping or MMIO, IRQ or reset query/action, DMA,
firmware, WFDMA, or radio operation.

Restore returned at `16:44:24.348499 IST`. The kernel had logged the ASIC at
`16:44:24.227849`, HW/SW firmware at `16:44:24.302869`, and WM firmware at
`16:44:24.313828`. iwd observed `phy10` at `16:44:25.136466` and final
`wlan10` at `16:44:25.720022`. With the supervisor's two-second sampling
resolution, the new durable BDF-owned `wiphy_ready` and BDF-owned,
iwd-queryable `usable_interface_ready` transitions were both recorded at
`16:44:26.391315`, 2.04 seconds after restore. Association, IPv4, default
route, and gateway connectivity were all sampled at `16:44:30.498662`;
measured restore-to-connectivity time was 6,197 ms. The watchdog disarmed with
`outcome=passed reason=none` at `16:44:30.564992`.

iwd's first attempt selected `ph1` at `16:44:28.378855`, completed the usual
status-77 retry, authenticated, and associated at `16:44:29.548866`; the AP
then immediately disassociated it with reason 2 (`PREV_AUTH_NOT_VALID`). iwd
returned to `autoconnect_full` without logging `association-timeout` or
`connect-failed`, selected the AP again after only 8 ms, and the second attempt
associated at `16:44:29.818879` and reached `connected` at
`16:44:29.959353`. Therefore neither strict failure condition fired: there was
no association timeout/connect-failed event and recovery was well below 60
seconds. Boot ID `cd298031-f2f5-4911-8c90-8d9e89bb40b8` remained unchanged;
final state was mt7921e in D0/runtime-active, iwd active, rfkill unblocked,
`wlan10` associated with IPv4/default route, gateway ping successful, and the
watchdog inactive. This control clears the anomalous-backoff question only; it
does not add evidence for any further hardware mutation.

### Next active-path boundary: conn-on ownership

The temporary physical early return remains before the general active-resource
path. A source trace of that path shows that after its persistent PCI INTx
disable and `MT_PCIE_MAC_INT_ENABLE = 0` store, constructing `VfioOwnership`
has no device effect. The first device operation in
`acquire_driver_ownership` is `write_clear_own`: one aligned volatile 32-bit
store of `0x00000002` (`PCIE_LPCR_HOST_CLR_OWN`) to direct BAR0 offset
`0xe0010`. The next operation is a volatile 32-bit read of the same offset,
polling `PCIE_LPCR_HOST_OWN_SYNC` clear. No IRQ installation, reset, DMA
mapping, firmware operation, WFDMA access, or radio operation is between the
interrupt-latch store and this ownership-command store. The general path has
already allocated unrelated DMA arenas before entering the mutation closure,
but an isolated boundary does not need them; it needs only BAR0 page
`0xe0000` read-write in addition to the already-proven preflight pages.

At pinned Linux commit `e8efe09d4f378992c890d181d65e2ed8d8cb1194`,
`mt792x_regs.h` defines physical `MT_CONN_ON_LPCTL = 0x7c060010`,
`PCIE_LPCR_HOST_SET_OWN = BIT(0)`, `CLR_OWN = BIT(1)`, and `OWN_SYNC = BIT(2)`.
The fixed map in `mt7921/pci.c::__mt7921_reg_addr` maps physical base
`0x7c060000` to BAR0 `0xe0000`, producing direct offset `0xe0010` without an
L1 selector transaction. `mt792x_core.c::__mt792xe_mcu_drv_pmctrl` writes
`CLR_OWN`, optionally waits 2--3 ms when PCIe ASPM is supported, and polls
`OWN_SYNC == 0` for 50 ms at 1 ms ticks, repeating for at most ten attempts.
This is Linux PCI transport ownership below Fuchsia SoftMAC, not a Fuchsia
firmware or radio contract.

The register is a command/status handshake, not an ordinary saved-value
latch. `CLR_OWN` requests host ownership; its command bit is not expected to
remain set. A read with either `SET_OWN` or `CLR_OWN` asserted is outside the
state Linux relies on. `OWN_SYNC == 0` is driver-owned and `OWN_SYNC == BIT(2)`
is firmware-owned. Unknown non-command bits are status, so rollback cannot be
specified as writing a saved full word. The pinned inverse operation is
`mt792xe_mcu_fw_pmctrl`: write only `PCIE_LPCR_HOST_SET_OWN`, then poll
`OWN_SYNC == BIT(2)` with the same ten 50 ms attempts. State restoration is
therefore equality of the initial ownership state, with command bits clear,
not raw full-word equality.

Required state is the already-established VFIO/iommufd attachment, D0,
Memory-Space Enable set, Bus Master Enable clear, persistent PCI INTx disable,
the PCIe MAC interrupt latch at zero, and a writable mapping of only conn-on
page `0xe0000`. Firmware/conn-infra must be powered enough to acknowledge the
handshake; the earlier isolated physical ownership run observed
`MT_CONN_ON_MISC = 1` (firmware power set), N9 readiness clear, DMA disabled,
and acquired driver ownership on the first write with status zero. That report,
`/var/lib/wifi-driver-lab/reports/20260802T170812Z-0000_05_00.0.log`, proves
the command alone on this adapter, but it restored only by VFIO function reset
and did not prove the inverse `SET_OWN` handshake or this exact composite
state.

The current helper is not ready for another physical claim. The target module
has `disable_aspm=N`, while `acquire_driver_ownership` currently polls
immediately and has no representation of Linux's conditional 2--3 ms ASPM
settling delay. Actual ASPM support is derived by Linux from both endpoint and
parent Link Control state, so it must not be guessed from that module parameter.
Also, current active containment and the earlier standalone run use VFIO reset,
not the pinned inverse ownership handshake, as rollback.

The smallest independently reversible next gate is consequently an isolated
ownership round trip, not the full active helper: read `MT_CONN_ON_LPCTL` once,
reject all ones or asserted command bits, and save only the semantic initial
`OWN_SYNC` state; if firmware-owned, issue only `CLR_OWN`, honor the pinned ASPM
delay conservatively, and poll only `OWN_SYNC` clear within the pinned bound;
then issue only `SET_OWN` and poll the original firmware-owned state before
unmapping. If initially driver-owned, the CLR command is idempotent and no SET
command may be issued because that would change the initial state. On every
exit after issuing CLR from an initially firmware-owned state, attempt SET and
the semantic-state poll before unmap even if the CLR poll failed, then restore
the exact saved interrupt latch and PCI Command.
Durable markers must precede every command, poll phase, inverse, and unmap.
This gate stops before WFSYS reset, WFDMA reads or writes, IRQ installation,
DMA mapping, firmware loading, or radio work. No hardware run is authorized
until the ASPM delay and inverse rollback are represented and separately
reviewed.

Those prerequisites are now represented offline, without making the temporary
gate reachable. `pcie_link_control` walks the conventional PCI capability list
only when `PCI_STATUS_CAP_LIST` is asserted, uses the masked pointer at config
byte `0x34`, finds capability ID `0x10`
(`PCI_CAP_ID_EXP`), and reads the little-endian Link Control word at capability
offset `0x10` (`PCI_EXP_LNKCTL`). It rejects short configurations, invalid or
looping capability pointers, a truncated PCIe capability, and capability
absence. `mt76_pci_aspm_supported` parses endpoint and optional parent bridge
independently, masks exactly `PCI_EXP_LNKCTL_ASPMC = 0x3`, and reproduces
pinned `mt76/pci.c`: the delay predicate is true if either side has L0s or L1
enabled. Unlike the kernel caller, which operates on known PCIe devices, the
offline parser retains malformed/absent-capability errors instead of guessing
false. Focused fixtures cover a chained endpoint capability, endpoint L0s,
parent-only L1, both sides disabled, absence, a list loop, and truncation.

`round_trip_driver_ownership` is a separate, currently uncalled transaction.
It emits a before-read durable-stage hook, reads one full low-power-control
word, rejects all ones and either asserted command bit, and saves only the
semantic `OWN_SYNC` state. Its CLR state machine preserves Linux's ten attempts,
50 ms per attempt and 1 ms polling. Each poll window gets its own deadline,
started after the command, settling delay, and durable callbacks, so MMIO,
scheduling, and fsync overhead cannot clip a later Linux poll window. The
zero-overhead worst case is 500 ms without ASPM and 530 ms with ten maximum
settling delays. When the parsed ASPM predicate is true it
invokes the transport's exact 2,000--3,000 us range hook
after every CLR store and before the first poll; the host adapter conservatively
sleeps the upper 3,000 us bound. Before-store, after-store, delay, before-read,
status, retry, success, and timeout events are exposed to the
durable stage adapter.

If the snapshot was driver-owned, CLR is allowed as an idempotent command and
the transaction never issues SET on success or error. If it was firmware-owned,
the CLR result is retained but cannot bypass rollback: SET plus the pinned
ten independently timed 50-ms polls for `OWN_SYNC` set runs after every
CLR-path result. A SET or rollback-read transport error is retained, but cannot
short-circuit the remaining bounded best-effort SET/poll sequence; this covers
an MMIO store that reached hardware despite an adapter error. Successful
rollback followed by failed acquisition returns the acquisition error;
successful acquisition followed by failed rollback returns the rollback error;
dual failure retains both in `AcquireAndRestore`. A rollback timeout emits no
`Complete` event and therefore cannot claim restored ownership. Injected tests
cover source delay and successful round trip, initially driver-owned no-SET,
post-CLR read failure with successful rollback, CLR timeout with successful
rollback, SET timeout without a completion claim, rollback-read continuation,
an ambiguous SET error whose status verifies restoration, and simultaneous
primary and rollback transport errors. The VFIO adapter admits SET only on BAR0 page
`0xe0000` at exact offset `0xe0010`. The guarded SAE preflight now invokes the
transaction immediately before its unchanged early return, while PCI INTx and
the PCIe MAC interrupt latch are disabled. Its fsynced stage adapter retains
only snapshot, command, delay, retry, terminal, and unmap milestones rather
than logging every poll read.

The single watchdog-contained run in
`/var/lib/wifi-driver-lab/reports/20260810T124821Z-0000_05_00.0.log` completed
with userspace `rc=0` and supervisor restore `failed=0`. Endpoint
`0000:05:00.0` plus parent `0000:00:02.2` selected the ASPM delay. The snapshot
was already driver-owned (`0x00000000`), so the source-correct transaction
issued one CLR, settled for the maximum 3 ms, verified driver ownership at
5 ms, and intentionally issued no SET. The PCIe MAC latch restored
`0x000000ff`, PCI Command restored `0x0002`, and all BAR mappings were removed.
The client lost SSH after launch, left the watchdog armed, and the watchdog
rebooted the host; the durable report survived and proves the successful gate
and native supervisor restoration. It does not physically prove SET because
the saved initial state did not authorize that inverse command.

### Next coherent boundary: reset plus IRQ ownership

The pinned `mt7921_pci_probe` continuation is one responsibility rather than a
sequence of more register gates: after identity it calls
`mt792x_wfsys_reset`, writes the WFDMA host interrupt enable to zero, writes
`MT_PCIE_MAC_INT_ENABLE = 0xff`, requests the PCI IRQ, and only then enters
`mt7921_dma_init`. `exercise_irq_reset_boundary` now represents that complete
pre-DMA boundary offline: it prevalidates the eventfd-capable vector, preserves
Linux's reset, host-mask, MAC-gate, then IRQ-install order, and stops with the
host interrupt mask still zero. It deliberately has no
DMA mapping, ring setup, firmware loading, or MCU command surface.

Containment is part of the same state machine: whether setup succeeds or an
IRQ installation error is ambiguous, it attempts host IRQ mask, MAC-source
disable, explicit VFIO IRQ disable, containment reset, and post-reset safe-state
verification in that order.
The primary error and every cleanup error remain separately visible. Durable
events cover only IRQ installation, the existing reset milestones, host mask,
MAC enable, setup completion, and each cleanup boundary. Focused fixtures prove
the successful source-derived order and that all cleanup steps still run after
an ambiguous install failure. The temporary early-return path now contains
only the minimal concrete adapter for this boundary.

The one guarded physical attempt is report
`/var/lib/wifi-driver-lab/reports/20260810T130511Z-0000_05_00.0.log`. It
prevalidated the 32-vector eventfd-capable MSI index, completed WFSYS
assert/release/readiness in 59 ms, left the host interrupt mask at zero, opened
the PCIe MAC gate, and installed the VFIO MSI eventfd before any BME, DMA,
firmware, WFDMA-enable, or radio work. Immediate cleanup masked host and MAC,
disabled the owned IRQ, completed VFIO reset, and passed the existing
post-reset host/MAC/DMA-disabled plus PCI BME-disabled verification. Supervisor
restore ended with `failed=0`.

Userspace returned failure only because the first adapter version redundantly
sent an explicit index-disable after its successfully owned IRQ had already
been disabled; VFIO correctly returned `EINVAL`, retained as cleanup entry
`DisableIrq`. The explicit index path is now used only when installation did
not return an owner, or as a best-effort fallback after owned-disable failure.
No second physical attempt was made. The completed safe-state verification is
authoritative for containment; the redundant cleanup error did not leave an
IRQ or device source active.

### Contained DMA-resource boundary

The next guarded boundary now allocates only the DMA resources needed to
represent Linux's pre-firmware ring setup. It maps four BAR pages and ten DMA
arenas: 4 KiB TX and RX guards, 4 KiB FWDL and MCU TX rings, 4 KiB MCU and WA
RX rings, two 16 KiB RX-buffer arenas, a 64 KiB command-payload arena, and a
4 KiB FWDL-payload arena (126,976 DMA bytes total). It programs the existing
18 TX, eight RX, and one WA RX ring slots while BME is clear and the WFDMA
enable/busy low nibble plus host and MAC interrupt masks are all zero.

Only after those mappings and disabled-state checks does the boundary set PCI
BME. WFDMA remains disabled throughout; the path has no firmware loader, MCU
publication, response interrupt, or radio operation. Cleanup masks host and
MAC sources, clears WFDMA's low nibble, clears BME, releases all DMA and BAR
mappings and the IOAS, then performs VFIO reset and the established
post-reset containment checks. If that verification cannot prove the safe
state, the resources remain parked under the watchdog rather than being
claimed released.

The single guarded physical attempt is report
`/var/lib/wifi-driver-lab/reports/20260810T131434Z-0000_05_00.0.log` and used
release binary SHA-256
`95148a063f6c7e5b53d02df1758828f028587577a5e7b0dfb2892b586ab1e827`.
It completed the preceding ownership and IRQ/reset boundaries, mapped all ten
arenas and four BAR pages, verified BME false and WFDMA disabled, prepared all
27 ring slots, then observed BME true with WFDMA still disabled. Cleanup
disabled BME, recorded resources unmapped before reset, passed the safe-state
verification, and ended with userspace `rc=0` and supervisor restore
`failed=0`. The client heartbeat briefly lost SSH and conservatively returned
unknown status, but the durable report contains every completion marker.
Afterward `mt7921e` was rebound, `iwd` was active, and `wlan1` was connected;
the reboot watchdog was inactive. No second attempt was made.

### Firmware-bootstrap boundary

The Linux-derived loader is separable immediately after N9 readiness and the
bounded `GET_NIC_CAPABILITY` response. `load_mt7921_firmware_bootstrap` follows
the existing NIC-power, download-ready, patch semaphore, patch scatter, RAM
scatter, firmware-start, and N9-ready sequence, then accepts exactly that one
post-N9 capability response and returns. The next command in the full path is
the EEPROM hardware-block read, so the bootstrap boundary issues no EEPROM,
CLC/calibration, channel-domain, scan, management-frame, or radio command.

The `--run-one-shot-fwdl` transport uses the established ownership, WFSYS
reset, global-ring, MSI eventfd, BME, WFDMA TX/RX, and dual MCU-response-ring
path. Its cleanup masks PCIe MAC and WFDMA interrupts, disables WFDMA, waits
for DMA idle, clears BME, disables the IRQ, unmaps every DMA arena, resets the
VFIO device, and verifies the established BME/WFDMA/host/MAC safe state before
releasing the remaining BAR and IOAS resources. Bootstrap dispatch and this
cleanup order have focused source-shape coverage; the loader fixture proves
that `GET_NIC_CAPABILITY` is followed by cleanup rather than EEPROM or CLC.

The pinned artifacts are patch SHA-256
`a276c06c2b772adb50b86639d33c82824ff4c21d617feb78caea74c040b873f6`
(build `20260224110909a`, platform `ALPS`, patch version `0xffffffff`) and RAM
SHA-256
`b94217a951518a9c14095765f367bc5dd7698f2dc033941d6f18fc2ebd6a2ab9`
(firmware `____010000`, build `20260224110949`, chip `0x0d`, five regions).

The single guarded attempt used release binary SHA-256
`281077fd9d258bd6c392d1a30b2e2dcd341f55365a63fb4eaa57c58d9f1ae797`;
its report is
`/var/lib/wifi-driver-lab/reports/20260810T132637Z-0000_05_00.0.log`.
The host stopped responding and the reboot watchdog recovered it, but the
report ends after `USERSPACE begin`: the existing general-path JSON output was
still buffered, so no firmware, DMA/IRQ, ready, or cleanup milestone became
durable. This attempt is therefore inconclusive and does not physically prove
firmware publication, an MCU response, or userspace cleanup. No second attempt
was initially made. After watchdog recovery `mt7921e` rebound, `iwd` was
active, `wlan0` was connected, and the watchdog was inactive. The native
driver independently reported the same patch build and WM firmware version
during recovery, but that is recovery evidence, not proof of the userspace
bootstrap. The coherent
bootstrap and cleanup markers now explicitly flush stdout for any future
authorized run.

One rerun was subsequently authorized after adding flushed phase markers for
transport readiness, patch completion, RAM completion plus firmware-start
acknowledgement, N9 readiness, NIC capability, and containment. It used binary
SHA-256
`2ae603a90af3123c8353327c5b7319b8e4557eafe16f7898c4027ebd186b193d`;
the report is
`/var/lib/wifi-driver-lab/reports/20260810T133639Z-0000_05_00.0.log`.
The host again stopped responding and watchdog-rebooted, and the exact last
durable milestone remained supervisor `USERSPACE begin`: none of the flushed
transport-ready or later markers was reached. Consequently the boundary is
blocked before proven transport readiness; there is no durable evidence that
BME, WFDMA, the MSI eventfd/source, firmware DMA, or MCU publication was
enabled during this run, and no userspace cleanup can be claimed. The
watchdog recovery again rebound `mt7921e`; `iwd` was active, `wlan0` connected,
and the watchdog inactive. This was the only authorized rerun.

### Contained pre-engine WFDMA preparation

The passed DMA-resource coordinator now also represents Linux's coherent
pre-engine transport preparation. With BME and WFDMA engines initially off it
sanitizes the global configuration, waits for idle, configures the extended
and DMASHDL bypass state, toggles the WFDMA index reset, programs the existing
global rings, installs the selected MSI eventfd, requires empty interrupt
status, and writes the source-derived queue configuration. It then enables
BME while leaving TX/RX engine bits clear and both host and PCIe MAC source
masks zero. No MCU/FWDL CPU index or firmware descriptor is published.

Cleanup retains the passed containment order: host and MAC masks zero, WFDMA
disabled and idle, BME clear, MSI disabled, all active DMA/BAR/IOAS resources
released, VFIO reset, and the established safe-state verification. Focused
source-shape coverage proves the preparation precedes BME, no engine-enable
construction or response-source mask is present, and unmap precedes reset.

The single guarded attempt used binary SHA-256
`5c1e64e3375095f44bcfef7938407f093c5fb6650ed789fe539a59c88b986b32`;
its report is
`/var/lib/wifi-driver-lab/reports/20260810T134859Z-0000_05_00.0.log`.
It durably reached both `vfio_wfdma_prep_begin` and
`vfio_wfdma_prep_complete` with engines, host IRQ, and MAC IRQ disabled, BME
enabled, and MSI owned. Immediate cleanup disabled BME and MSI, released the
resources before reset, and passed `vfio_dma_safe_state_verified`. Userspace
ended with `rc=0` and supervisor restore with `failed=0`. The wrapper lost its
SSH heartbeat and conservatively left watchdog recovery armed, but the durable
report is complete. After recovery `mt7921e` was rebound, `iwd` active,
`wlan0` connected, and the watchdog inactive. No follow-on attempt was made.

### Contained transport activation without firmware

The same contained coordinator now continues from passed pre-engine state
through the remaining Linux pre-firmware transport activation as one boundary:
it enables WFDMA TX/RX with the source-derived global configuration, opens the
PCIe MAC and WM/WM2 host response sources, acquires top-driver ownership,
disables PCIe L0s, and selects normal SWDEF mode. It does not publish an MCU,
FWDL, or firmware-ring CPU index, construct a loader, or issue an MCU command.

Containment immediately masks the host and MAC sources, clears the WFDMA
engine and related configuration bits, waits for DMA idle, clears BME,
disables MSI, releases resources, resets VFIO, and verifies the established
safe state. Focused source-shape coverage proves activation precedes masking
and containment, and rejects every firmware publication primitive.

The single guarded attempt used binary SHA-256
`c3a5038a902e356fcf18dfd9d1f050453882d78d308b73d30474251832763d62`;
its report is
`/var/lib/wifi-driver-lab/reports/20260810T135553Z-0000_05_00.0.log`.
It durably completed `vfio_wfdma_activation_begin` and
`vfio_wfdma_activation_complete` with engines enabled, WM/WM2 and MAC sources
enabled, top ownership acquired, L0s disabled, SWDEF normal, and firmware
publication false. Immediate containment disabled BME, released mappings
before reset, and passed `vfio_dma_safe_state_verified`. Userspace ended with
`rc=0` and supervisor restore with `failed=0`. After the conservative wrapper
heartbeat loss, native `mt7921e` rebound, `iwd` was active, `wlan1` connected,
and the watchdog inactive. No firmware attempt followed.

### Contained firmware bootstrap on the proven transport

The firmware bootstrap now enters the same contained coordinator used by the
passed transport-activation boundary. It verifies and parses both installed
artifacts before VFIO attachment, retains the coordinator's live MSI, WFDMA,
WM/WM2, command, and firmware-download resources after activation, and gives
those resources directly to the existing loader. There is no parallel
transport setup. Durable coarse markers cover process start, artifact
readiness, and transport readiness before the existing patch, RAM,
firmware-start, N9-ready, and NIC-capability phases.

The single guarded attempt used binary SHA-256
`23581dbfcb72fa50f9a7a09bc8a86b21ca471e3c174390ab45322cc1fd5416f5`;
its report is
`/var/lib/wifi-driver-lab/reports/20260810T140636Z-0000_05_00.0.log`.
It completed one patch section and four downloadable RAM regions in 196
scatter chunks, observed N9 ready, and received a 23-element
`GET_NIC_CAPABILITY` response on WM2. The boundary then quiesced the
transport without issuing EEPROM, CLC/calibration, channel, scan, or radio
operations. Outer containment masked and disabled the transport, cleared BME,
released mappings before reset, and passed the safe-state verification.
Userspace ended with `rc=0` and supervisor restore with `failed=0`. The
wrapper conservatively returned unknown status after losing SSH, but the
durable report is complete. After recovery `mt7921e` was rebound, `iwd`
was active, `wlan1` was connected, and the watchdog was inactive. No second
attempt was made.

### Contained passive EEPROM and CLC initialization

The same consolidated transport now continues from NIC capability discovery
through the next Linux-derived passive responsibility. It reads the fixed
`0x550` eFuse/EEPROM hardware block, selects the installed CLC calibration
record using that result and the discovered chip capability, and applies the
single world/indoor rule. `SET_CLC` only supplies regulatory/calibration data;
the separately gated channel-domain call remains unreachable, as do channel
tuning, MAC/RF enable, scan, management TX, and SAE. Phase markers for eFuse
acquisition and CLC configuration are flushed before the existing mandatory
transport quiesce and outer containment.

The single guarded attempt used binary SHA-256
`4da54784290d782f05ed54e2c23c92e782fc258a844031dd8adab7a0a1866a47`;
its report is
`/var/lib/wifi-driver-lab/reports/20260810T141101Z-0000_05_00.0.log`.
After the previously proved 23-element NIC capability response, eFuse event
`0xed` returned on WM2 with `valid=0`. One response-bearing world/indoor
CLC rule then completed as event `0x80`, with special-UNII mask zero. The
summary records one patch section, four downloadable RAM regions, 196 scatter
chunks, one applied CLC rule, and explicitly records channel, scan, management
TX, SAE, and radio as false. Transport quiesce and outer containment cleared
BME, released mappings before reset, and passed safe-state verification.
Userspace ended with `rc=0` and supervisor restore with `failed=0`. The
wrapper conservatively returned unknown after losing SSH, but the durable
report is complete. Native `mt7921e` rebound, `iwd` was active, `wlan1`
was connected, both watchdog units were inactive, and no lab state directory
remained. No second attempt was made.

### Consolidated receive-only channel-1 boundary

The channel-1 passive operation now enters the same contained transport and
configuration path. After world/indoor CLC it sends the separately gated
39-channel `NO_IR` domain, owns data RX ring 2 and its interrupt, applies the
source-exact passive MAC plan, tunes channel 1, and requests one passive dwell
with zero SSIDs and zero probes. The existing pinned Fuchsia processing path
must parse at least one real beacon or probe response and observe matching scan
completion within the bounded dwell/deadline. The contained source has no
active-scan, probe-frame, management/data-frame, rate-power, or SAE publication
call and does not allocate the SAE TX arenas.

The one guarded attempt used binary SHA-256
`fad60861c85997913a8e5eede2f41d08f9d6ec177edd3641be1a11999541cf3a`;
its report is
`/var/lib/wifi-driver-lab/reports/20260810T141847Z-0000_05_00.0.log`.
Firmware, NIC capability, eFuse, CLC, and the channel-domain TX completion all
passed. The passive hook then stopped before MAC setup, channel tune, or scan
because the newly consolidated coordinator had not advanced its containment
phase from `Contained` to `DmaAndResponseIrqEnabled`. Thus this run proves
no receive observation and performed no intentional RF transmission.
Transport quiesce and outer containment still disabled BME, released mappings
before reset, and passed safe-state verification; userspace ended with
`rc=1` and supervisor restore with `failed=0`. Native `mt7921e` rebound,
`iwd` was active, `wlan0` was connected, both watchdogs were inactive, and
no lab state remained.

The coordinator now records the contained DMA-disabled and
DMA/response-enabled phases explicitly and passes the actual passive operation
into shared resource acquisition, so the existing passive BAR pages and data
RX arenas are retained for the hook. All 73 integrated tests and the release
build pass after that correction (release SHA-256
`ae7dda3efc61165aa97ab418b8e1ca2861b824ece8306f3f5dfd452c587724a7`).
Per the one-run limit, the corrected path has not been rerun physically.

One corrected rerun was subsequently authorized, using unchanged release
SHA-256
`ae7dda3efc61165aa97ab418b8e1ca2861b824ece8306f3f5dfd452c587724a7`.
Its report is
`/var/lib/wifi-driver-lab/reports/20260810T142520Z-0000_05_00.0.log`.
All passive preparation steps passed, including data RX ring/IRQ ownership.
The source-exact MAC enable, RX-path, device/BSS, receive-filter, and channel
switch commands completed for channel 1 at 2412 MHz, and the durable setup
marker records `intentional_tx=false`. The passive `START_SCAN` command
completed with matching scan ID 1 and firmware reported successful scan
completion, but the bounded dwell produced zero beacon/probe-response
observations. Therefore there is no RSSI evidence and the receive boundary is
not physically proved.

The run stopped at that exact bounded failure without retrying or transmitting
a probe, management frame, data frame, or SAE frame. Transport quiesce and
outer containment disabled WFDMA/IRQs/BME, released mappings before reset, and
passed safe-state verification. Userspace ended with `rc=1` and supervisor
restore with `failed=0`; native `mt7921e` rebound, `iwd` was active,
`wlan1` was connected, and both watchdog units were inactive. No further run
was made.

A later read-only native check, without VFIO detach, established that the
currently connected AP was not on channel 1: iwd reported interface `wlan0`,
BSSID `42:50:fd:67:3a:88`, 5180 MHz/channel 36, and RSSI -57 dBm. The same
zero-TX contained operation was therefore minimally parameterized with a
bounded channel argument and a 150--250 ms passive dwell. All 73 integrated
tests and the release build passed; the unchanged-semantics binary SHA-256 was
`9f5f96922b57d2c0941d27e5658bf956f886205cf61cfdf685cb423a564ae8f1`.

The single environment-informed attempt is report
`/var/lib/wifi-driver-lab/reports/20260810T143132Z-0000_05_00.0.log`.
Every passive preparation and configuration command completed for channel 36
at 5180 MHz, and the durable marker records the 150--250 ms dwell plus
`intentional_tx=false`. Scan ID 1 again completed successfully but produced
zero parsed beacon/probe-response observations, so the Fuchsia adapter has no
physical RSSI evidence and the receive boundary remains unproved. No probe,
management, data, or SAE frame was transmitted.

The run stopped without retry. Transport quiesce and outer containment
disabled WFDMA/IRQs/BME, released mappings before reset, and passed safe-state
verification; userspace ended with `rc=1` and supervisor restore with
`failed=0`. Native networking recovered on `wlan1` to the same BSSID,
5180 MHz/channel 36, with RSSI -60 dBm (average -58 dBm); both watchdogs were
inactive and the lab state directory was empty. No further attempt was made.

### Source-exact RX ring lifecycle boundary

The data RX arena is now prepared and fenced, and ring 2's base, count, CIDX,
and DIDX are published while RX DMA is disabled. That identity remains live
through firmware startup; passive preparation only verifies it and enables the
data IRQ.

The single guarded channel-36 run passed. Report
`/var/lib/wifi-driver-lab/reports/20260810T145425Z-0000_05_00.0.log`
records a 308-byte normal RX frame routed from ring 4 and a Fuchsia
`ScanObservation::Beacon` for BSSID `42:50:fd:67:3a:88` on channel 36 at
-52 dBm. Scan completion reported one observation with zero intentional TX.
Transport cleanup quiesced DMA and IRQs, disabled BME, reset VFIO into the
verified safe state, and returned `rc=0`; supervisor restore reported
`failed=0`, with native `mt7921e` and iwd healthy on the same boot. No
further physical run was made.

### Consolidated SAE routing correction and guarded result

`RunOneShotSaeAuth` no longer selects the earlier contained-DMA milestone,
which accepted only the firmware and single-channel passive operations and
returned before the consolidated loader. A regression test now requires SAE
to bypass that early gate while remaining a firmware-loading active-MCU
operation. All 76 integrated tests and the release build passed; the binary
SHA-256 was
`6d3ed422c3e817d71ad4f800710f29ecf291cd31624b2371085bf628a4fae560`.

Exactly one corrected protected-FD attempt was launched under the transient
`wifi-sae-exchange3` supervisor. Its report is
`/var/lib/wifi-driver-lab/reports/20260810T152400Z-0000_05_00.0.log`.
The durable report reached userspace after VFIO handoff, but the last SAE stage
was only `credential_read`: there is no firmware, passive RX, SAE commit, or RF
TX evidence. The process did not return before recovery; the kernel recorded a
60-second page-pool shutdown stall with six inflight buffers and the external
watchdog rebooted the host. After reboot, native `mt7921e` was rebound, `iwd`
was active, both watchdog units were inactive, and no lab state remained. This
is not an SAE pass and no second attempt was made.

### Deferred-DMA SAE prerequisite result

The deferred management-DMA correction was content-transplanted onto the
newer passive-RX and Netstack host lineage in commit `5846e550244a`. All 77
physical-transport tests and the locked release build passed; the integrated
binary SHA-256 was
`a6f6cac8ed4241a428bd6073c078255488e2e00844a347b8c5660d2e4accde5a`.

The first guarded launch, `wifi-sae-exchange4`, did not execute the binary:
the reviewed inner launcher's `/usr/bin/env bash` shebang was incompatible
with the recovery wrapper's sanitized payload environment. Report
`/var/lib/wifi-driver-lab/reports/20260810T154601Z-0000_05_00.0.log` records
`USERSPACE end rc=127` and `RESTORE end failed=0`. Native networking recovered,
and no firmware or radio claim follows from that launcher failure.

One launcher-only corrected retry used the same binary, credential FD, and
supervisor with absolute `/run/current-system/sw/bin/bash`. Report
`/var/lib/wifi-driver-lab/reports/20260810T155130Z-0000_05_00.0.log` reached
`USERSPACE begin`; the last durable SAE stage was only `credential_read`.
There is no `watchdog_verified`, firmware, passive-RX, SAE-commit, or RF-TX
evidence. The kernel then reported a 60-second page-pool shutdown stall for
pool 19 with six inflight buffers, and the external watchdog rebooted the host
before userspace end or supervisor restore could be recorded. After reboot,
native `mt7921e` rebound, iwd was active, `wlan0` had carrier, all recovery and
watchdog units were inactive, and `/run/wifi-driver-lab` was absent. The
corrected retry is not an SAE pass, and no further attempt was made.

An offline audit found that `credential_read` did not bound the failure to the
following host checks. SAE and the passing passive operation execute the same
BDF/VFIO environment parsing, synchronous sysfs identity reads, and external
watchdog status command in the same order. When SAE was removed from the early
contained-transport gate, however, the same predicate also accidentally
suppressed `watchdog_verified` and the VFIO open/bind/attach stage markers.
Those checks remain inside the payload because watchdog state must be verified
at the handoff point and each call is locally bounded by a regular file read or
the supervisor's local status operation. A separate stage-recording predicate
now includes SAE without restoring the early return, so future durable
evidence can distinguish host preflight from VFIO acquisition without adding
new instrumentation.

Exactly one diagnostic run of that marker correction was launched as
`wifi-sae-exchange6`. Its report is
`/var/lib/wifi-driver-lab/reports/20260810T160250Z-0000_05_00.0.log`; the exact
last durable stage is `vfio_attach_iommufd_pt_after`. This proves that the
shared environment parsing, PCI identity checks, external-watchdog check,
VFIO cdev and iommufd opens, iommufd bind, IOAS allocation, and device attach
completed. It does not prove the subsequent BAR-region query or mappings,
firmware startup, passive RX, SAE commit, or RF TX.

The kernel later reported a 60-second page-pool shutdown stall for pool 19
with seven inflight buffers, and the watchdog rebooted before userspace end or
supervisor restore could be recorded. After reboot, native `mt7921e` rebound,
iwd was active, `wlan0` had carrier, all recovery and watchdog units were
inactive, and `/run/wifi-driver-lab` was absent. No further physical mutation
was attempted.

The immediate source-order comparison found one concrete mismatch at that
boundary. The physically passing bounded channel-36 passive operation
re-verified PCI MSE=1, BME=0, and D0 immediately after attach, then recorded
`vfio_attached_d0_preflight_already_ready` before its first device-info query.
SAE skipped that post-attach revalidation and proceeded directly to an
unmarked `VFIO_DEVICE_GET_REGION_INFO(BAR0)` ioctl. Its credential, target,
and optional management-DMA values remain live but are neither borrowed nor
dropped in this segment, so they cannot explain a stall at this boundary. The
smallest correction moves the existing post-attach D0 check ahead of the
contained/passive branch for both stage-recording paths; SAE now refuses the
BAR query unless the same passing precondition holds and emits the existing
marker.

One guarded run of that correction was launched as `wifi-sae-exchange7` from
commit `4c9474d3778e`; the release binary SHA-256 was
`f57edddaf296df9c5713a3fc1d7b4f29ebe29166e2533453477f3b169fa2e4d1`.
Report `/var/lib/wifi-driver-lab/reports/20260810T161611Z-0000_05_00.0.log`
ended at `vfio_attached_d0_preflight_already_ready`. The shared post-attach
MSE=1, BME=0, and D0 reread therefore passed, but there is no evidence for the
following BAR-region query or mappings, firmware startup, beacon RX, SAE TX,
or SAE RX. The kernel later reported a 60-second page-pool shutdown stall for
pool 19 with six inflight buffers, and the watchdog rebooted before userspace
end or supervisor restore could be recorded.

After reboot, native `mt7921e` rebound, iwd was active, `wlan0` had carrier,
all recovery and watchdog units were inactive, and `/run/wifi-driver-lab` was
absent. No further physical mutation was attempted.

The existing marked `VFIO_DEVICE_GET_INFO` transaction was then moved into the
same shared active path, with contained passive reusing its result. One guarded
run of that correction was launched as `wifi-sae-exchange8` from commit
`efc2880fe59c`; the release binary SHA-256 was
`2899307651653dffcfee56b22a339ff2f64421b4e062e5299a35e582be93583e`.
Report `/var/lib/wifi-driver-lab/reports/20260810T162609Z-0000_05_00.0.log`
ended at `vfio_device_get_info_after argsz=24 flags=0x3 num_regions=9
num_irqs=5`. Device-info discovery therefore completed successfully, but
there is no evidence for the following region/BAR query or mappings, firmware
startup, beacon RX, SAE TX, or SAE RX. The offline UNI/key transport was not
exercised.

The kernel later reported a 60-second page-pool shutdown stall for pool 19
with two inflight buffers, and the watchdog rebooted before userspace end or
supervisor restore could be recorded. After reboot, native `mt7921e` rebound,
iwd was active, `wlan0` had carrier, all recovery and watchdog units were
inactive, and `/run/wifi-driver-lab` was absent. No further physical mutation
was attempted.

Region enumeration was then moved into the same shared active path, with both
later branches reusing the discovered BAR0 descriptor. One guarded run of that
correction was launched as `wifi-sae-exchange9` from commit `feb3e9fe6456`;
the release binary SHA-256 was
`5e8f9026c83312f7c4854ed6216df3f757985bd8a911e13c2bc7192f739a424c`.
Report
`/var/lib/wifi-driver-lab/reports/20260810T163738Z-0000_05_00.0.log`
ended at `vfio_region_discovery_complete`. All nine advertised regions were
therefore queried and BAR0 was identified, but there is no evidence for the
following BAR mappings, firmware startup, beacon RX, SAE TX, or SAE RX. The
offline UNI/key transport was not exercised.

The watchdog rebooted before userspace end or supervisor restore could be
recorded. After reboot, native `mt7921e` rebound, iwd was active, `wlan0` had
carrier, all recovery and watchdog units were inactive, and
`/run/wifi-driver-lab` was absent. No further physical mutation was
attempted.

The per-stage file write and blocking `sync_all` were then removed; stages
were emitted best-effort to stderr instead. One guarded run of that correction
was launched as `wifi-sae-exchange10` from commit `23191175e221`; the
release binary SHA-256 was
`73ddf24e4866d9cc2e835c3da5baae643621a9a3ddd954a0c22f90881e8a3adc`.
Report
`/var/lib/wifi-driver-lab/reports/20260810T165813Z-0000_05_00.0.log`
contains only the supervisor's member, start, and userspace-begin records:
`wifi-driver-lab` did not spool the payload's stderr and the prior-boot
journal retained no transient-unit output. Consequently this run provides no
durable evidence for BAR mapping, firmware startup, beacon RX, or SAE TX/RX;
association, key, and data effects remained disabled.

The kernel recorded two completed `vfio-pci` resets followed by a 60-second
page-pool shutdown stall for pool 19 with one inflight buffer. The watchdog
rebooted before userspace end or supervisor restore could be recorded. After
reboot, native `mt7921e` rebound, iwd was active, `wlan0` had carrier, all
recovery and watchdog units were inactive, and `/run/wifi-driver-lab` was
absent. No further physical mutation was attempted.

Credential ingestion was subsequently bounded by an explicit validated byte
length and `read_exact`, eliminating any dependency on pipe EOF. A separate
transient companion also synchronized the newest report once per second,
outside the payload control group. One guarded run was launched as
`wifi-sae-exchange11` from commit `6d5348ffd6fa`; the release binary SHA-256
was `9bd501540921cefb4e9e4e7fa124378d7aca8b01dde42e6ac467ac6e12a36833`.
Even with the independent durability companion, report
`/var/lib/wifi-driver-lab/reports/20260810T172942Z-0000_05_00.0.log` ends at
the harness `USERSPACE begin` record and contains no first payload stage.

The kernel recorded two completed `vfio-pci` resets and then a 60-second
page-pool shutdown stall for pool 19 with two inflight buffers. The durable
evidence localizes the stop only to the native-to-VFIO handoff or the payload's
pre-marker work; absence of the first marker alone cannot distinguish harness
handoff from pre-marker ACPI, argument, or credential processing. There is no
evidence for BAR mapping, firmware startup, beacon RX, or SAE TX/RX, and
association, key, and data effects remained disabled. The watchdog rebooted;
native `mt7921e`, iwd, and `wlan0` carrier recovered on the next boot, with no
active recovery or report-sync units and no `/run/wifi-driver-lab` state. No
further physical mutation was attempted.

A subsequent no-hardware isolation invoked the same release binary directly as
root, outside `wifi-driver-lab`, with a dummy declared eight-byte credential
and both PCI-BDF and VFIO environment absent. It returned in 24 ms, emitted
`credential_read`, and then failed with the expected
`DRV_PCI_BDF is required` error. Native `mt7921e`, iwd, carrier, and lab
state were unchanged. This excludes the target's ACPI scan, bounded credential
read, and direct exec path as the exchange-11 stall, leaving the
`wifi-driver-lab` native-to-VFIO handoff and its page-pool shutdown as the
active blocker.

A guarded run after deploying the proposed pre-unbind quiesce wrapper was
launched as `wifi-sae-exchange12` from commit `6d5348ffd6fa`; the release
binary SHA-256 was
`9bd501540921cefb4e9e4e7fa124378d7aca8b01dde42e6ac467ac6e12a36833`.
Report
`/var/lib/wifi-driver-lab/reports/20260810T175018Z-0000_05_00.0.log`
records VFIO acquisition, BAR discovery, firmware patch and RAM startup, NIC
capability, EEPROM and CLC configuration, passive receive preparation, and
five bounded channel-36 scans. Two scans received beacons from
`f2:a3:18:4f:30:76`, but the configured target was
`42:50:fd:67:3a:88`; the run therefore stopped before SAE with
`target beacon did not authorize channel 36`.

Cleanup disabled bus mastering, quiesced the transport, released DMA mappings,
reset the VFIO device, verified the safe state, and restored the native driver
with `RESTORE end failed=0`. Association, key, and data effects remained
disabled. The report contains no `QUIESCE` netdev line: review found that the
wrapper compared each resolved netdev device path with an unresolved
`/sys/bus/pci/devices` symlink, so the new netdev gate matched nothing and
vacuously succeeded. The later page-pool warning occurred during the watchdog
reboot after the already-successful native restore; the supervisor deliberately
kept the watchdog armed because the experiment returned nonzero. Native
`mt7921e`, iwd, and carrier recovered on the next boot. No further physical
mutation was attempted pending a canonicalized, nonempty quiesce match.

The canonicalized quiesce wrapper, dynamic native target selection, and exact
integer-or-`.0` frequency normalization were then deployed for one guarded
run as `wifi-sae-exchange13`. The run used commit `d73b1f4e462c`, release
SHA-256
`fc9d4b96230e24c61ccd27935404fea4da1c15ca19f406b0720190d4387ff5cf`,
and freshly selected target `f2:a3:18:4f:30:76` on channel 36 at 5180 MHz.
Report
`/var/lib/wifi-driver-lab/reports/20260810T181358Z-0000_05_00.0.log`
contains `Cannot find device "wlan0"` before any `QUIESCE` or
`USERSPACE` record. The interface disappeared between sysfs enumeration and
`ip link set dev wlan0 down` after iwd stopped. The wrapper failed closed
before native unbind or VFIO, and restoration completed with
`RESTORE end failed=0`.

The restored daemon did not reassociate within the bounded recovery window, so
the safety proof correctly remained incomplete and the watchdog rebooted.
There was no page-pool warning and no firmware, SAE, association, key, or data
operation. Native `mt7921e`, iwd, and carrier recovered after reboot. That
reboot also exposed a deployment regression: the selected persistent system
no longer exported the patched
`/sys/module/mt7921e/parameters/keep_d0_on_remove` parameter. Further
physical work is blocked on both a disappearance-safe netdev quiesce and a
system closure containing the patched kernel module.

After booting a merged closure with the patched kernel module and a
disappearance-convergent quiesce loop, one guarded run was launched as
`wifi-sae-exchange14`. The run used commit `d73b1f4e462c`, release SHA-256
`fc9d4b96230e24c61ccd27935404fea4da1c15ca19f406b0720190d4387ff5cf`,
and freshly derived target `f2:a3:18:4f:30:76` on channel 36 at 5180 MHz.
Report
`/var/lib/wifi-driver-lab/reports/20260810T182726Z-0000_05_00.0.log`
records the expected race-safe `QUIESCE netdev=wlan0 vanished=true`, VFIO
userspace entry, immediate userspace return with status 1, and successful
native restore. It contains no payload stage, firmware, SAE, association, key,
or data evidence.

The dynamic BSSID and channel were exported by the outer recovery supervisor,
but the handoff wrapper creates a nested transient service. The protected
inner launcher therefore received neither value and exited its validation
before invoking the binary; the earlier hard-coded target had hidden this
environment-transport boundary. The restored interface did not reassociate
within the bounded recovery window, so the watchdog rebooted. There was no
page-pool warning. The patched system closure remained selected after reboot,
`keep_d0_on_remove` was `Y`, and native `mt7921e`, iwd, and carrier
recovered. Further physical work is blocked on explicit non-secret target
transport across the nested service boundary.

After deploying explicit validated BSSID and channel forwarding into the
nested transient service, one guarded run was launched as
`wifi-sae-exchange15`. The run used commit `d73b1f4e462c`, release SHA-256
`fc9d4b96230e24c61ccd27935404fea4da1c15ca19f406b0720190d4387ff5cf`,
and freshly derived target `f2:a3:18:4f:30:76` on channel 36 at 5180 MHz.
Report
`/var/lib/wifi-driver-lab/reports/20260810T184002Z-0000_05_00.0.log`
records `QUIESCE netdev=wlan0 vanished=true`, payload entry, firmware patch
and RAM startup, passive receive preparation, a target beacon and channel-gate
pass, and `sae_tx_resources_acquired after_beacon=true
after_rate_power=true`. This establishes that the infrastructure and hardware
path reaches target-gated SAE management-TX resource acquisition.

The first SAE management-TX attempt was rejected locally with
`DeviceOps SAE TX rejected: ACCESS_DENIED`, before actual SAE frame
publication. No SAE commit or confirm was transmitted or received. Userspace
returned status 1, then cleanup quiesced the transport, released DMA mappings,
reset the VFIO device, verified the post-reset safe state, and restored the
native driver with `RESTORE end failed=0`. Native `mt7921e`, D0, iwd,
association, IPv4, the default route, and gateway connectivity recovered in
4155 ms on the renamed `wlan1`; the watchdog was disarmed without a reboot.
There was no page-pool warning or kernel warning/oops. The patched system
closure and profile remained selected, `keep_d0_on_remove` remained `Y`, and
the persistent default remained generation 25. Further physical work is
blocked solely on the pre-port SAE management-TX authorization/effect fix.

The next guarded attempt, launched as `wifi-sae-exchange16`, did not exercise
the authorization change. Although the integrated transport, adapter, and
netstack tests passed, the staged release artifact was the
`mt7921-passive-scan` package binary (SHA-256
`cad2920e1e176b13c59803fc6cf4d68d34a57428c34ba8c3183f72cf9dc79c52`)
rather than the feature-enabled `mt7921-vfio-read` artifact expected by the
launcher. Report
`/var/lib/wifi-driver-lab/reports/20260810T185109Z-0000_05_00.0.log`
records the correct dynamic target and
`QUIESCE netdev=wlan1 vanished=true`, followed by
`unknown argument --run-one-shot-sae-auth` and userspace status 1. There is
no payload, firmware, beacon, SAE commit or confirm TX/RX, or PMK evidence.

Restoration completed with `RESTORE end failed=0`. Native `mt7921e`, D0,
iwd, WPA3 association, IPv4, the default route, and gateway connectivity
recovered in 6224 ms on the renamed `wlan2`; the watchdog was disarmed
without a reboot. There was no page-pool warning or kernel warning/oops, lab
state was empty, and the patched system, profile, kernel, generation-25
default, and `keep_d0_on_remove=Y` invariants remained intact. No rerun was
attempted.

A corrected guarded attempt was launched exactly once as
`wifi-sae-exchange17` from integrated commit `e0a1b02053bf`. The explicitly
feature-enabled physical artifact had matching local and staged SHA-256
`c7f71a61c84ffa706e9521be3e174522c1fa73d56bcc6d7fcdf1f83fff61f6c5`;
before handoff, that exact staged binary passed a no-hardware CLI gate by
recognizing `--run-one-shot-sae-auth` and stopping at its missing-BSSID
validation. Report
`/var/lib/wifi-driver-lab/reports/20260810T185758Z-0000_05_00.0.log`
records the dynamic target, race-safe quiesce, payload and firmware startup,
target beacon and channel gate, and SAE TX resource acquisition.

The client-authorized SAE commit frame and descriptor were published to
management ring 0 and its producer index was advanced. No correlated TX-free
and TX-status completion arrived within the three-second bound, so the run
stopped with `SAE management TX completion timed out; frame may have
transmitted`. Commit transmission over the air and acknowledgement are
therefore unproven. There is no commit RX, confirm publication or RX, PMK
derivation, or authenticated marker. The management ring was stopped and
reset, then cleanup quiesced the transport, released DMA mappings, reset VFIO,
verified the post-reset safe state, and restored the native driver with
`RESTORE end failed=0`.

Native `mt7921e`, D0, iwd, WPA3 association, IPv4, the default route, and
gateway connectivity recovered in 20590 ms on the renamed `wlan3`; the
watchdog was disarmed without a reboot. The patched system, profile, kernel,
generation-25 default, `keep_d0_on_remove=Y`, and empty lab-state invariants
remained intact. Sixty seconds later the kernel reported one
`page_pool_release_retry` stall for pool 25 with one inflight buffer, while
native connectivity remained healthy. No rerun was attempted.

After replacing the global WFDMA logic reset with the bounded TX-ring-0 DTX
pointer reset, one guarded run was launched as `wifi-sae-exchange18` from
integrated commit `91a834f3f45f`. The explicitly feature-enabled release and
staged artifact shared SHA-256
`b3de64f27e6603529832a64948615259736cbcd527b536fd7a6147230290671e`.
Report
`/var/lib/wifi-driver-lab/reports/20260810T191634Z-0000_05_00.0.log`
records the dynamic target, firmware and beacon gates, SAE TX resource
acquisition, commit publication, and the first post-publication IRQ evidence:
`interrupt_status=0x0c400010`, including TX-done bit 4.

Completion handling then misrouted RX-ring-4 descriptor 7 into the MCU
response parser. Its control word was `0xc0280000`, descriptor length was 40,
and the attempted MCU header length was 2039, producing
`parse MCU response: InvalidLength`. No individual TX-free or TX-status
correlation became durable, so commit acknowledgement remains unproven. There
is no peer commit RX, confirm publication or RX, PMK derivation, or
authenticated marker.

Containment and cleanup quiesced the transport, released DMA mappings, reset
VFIO, verified the post-reset safe state, and restored the native driver with
`RESTORE end failed=0`. Native `mt7921e`, D0, and iwd returned, but
association timed out throughout the bounded recovery window. The watchdog
therefore remained armed and rebooted the host. The old boot also recorded one
`page_pool_release_retry` stall for pool 28 with three inflight buffers.
After reboot, the patched system, profile, kernel, generation-25 default, and
`keep_d0_on_remove=Y` invariants remained intact; native WPA3 association,
carrier, and connectivity were healthy on `wlan0`, with no lab state. No
rerun was attempted.

After adding bounded descriptor-consumption proof and demultiplexing management
completions before MCU response parsing, one guarded run was launched as
`wifi-sae-exchange19` from integrated commit `6107c58002b3`. The explicitly
feature-enabled release and staged artifact shared SHA-256
`5ea552601bc3b44cc6156f44e073790c85c4f2fcba6c193cf40a07d2fa3ef3ac`.
Report
`/var/lib/wifi-driver-lab/reports/20260810T193355Z-0000_05_00.0.log`
records the dynamic target, firmware and beacon gates, commit publication, and
`sae_tx_ring0_consumed didx=1 descriptor_done=true`.

The post-publication IRQ was `0x0c400010`. Ring 4 then delivered and routed
an acknowledged TX status for WCID 19 and PID 3, followed by TX-free for WCID
19, token 0, with `dropped=false` and one attempt. Their dual correlation
produced `sae_commit_tx_acked`, the first fully proven local SAE commit
transmission. The bounded peer-response wait received no matching SAE commit,
so the run stopped with `bounded SAE peer response timed out`. There is no
peer commit RX, confirm publication or RX, PMK derivation, or authenticated
marker.

Containment and cleanup quiesced the transport, released DMA mappings, reset
VFIO, verified the post-reset safe state, and restored the native driver with
`RESTORE end failed=0`. Native `mt7921e`, D0, iwd, WPA3 association, IPv4,
the default route, and gateway connectivity recovered in 20608 ms on the
renamed `wlan1`; the watchdog was disarmed without a reboot. The patched
system, profile, kernel, generation-25 default, `keep_d0_on_remove=Y`, and
empty lab-state invariants remained intact. There was no page-pool warning or
kernel warning/oops. No rerun was attempted.

After selecting SAE H2E from the peer RSNXE, one guarded run was launched as
`wifi-sae-exchange20` from integrated commit `a5dbf144e1ca`. The explicitly
feature-enabled release and staged artifact shared SHA-256
`1a38e18d8156a4bcde5c77525fbaa936cce04672e0932b72f929c4f15fe500d8`.
Report
`/var/lib/wifi-driver-lab/reports/20260810T195257Z-0000_05_00.0.log`
records peer and local H2E capability inputs selecting the reviewed Direct
path, the correctly generated H2E commit publication, and
`sae_tx_ring0_consumed didx=1 descriptor_done=true`.

The post-publication IRQ was `0x0c400010`. Ring 4 delivered an acknowledged
TX status for WCID 19 and PID 3, followed by TX-free for WCID 19, token 0,
with `dropped=false` and one attempt. Their dual correlation produced
`sae_commit_tx_acked`, proving local transmission of the H2E commit. The AP
gave no SAE response and there was no later RX IRQ, so the run stopped with
`bounded SAE peer response timed out`. There is no peer commit RX, confirm
publication or RX, PMK derivation, or authenticated marker.

Containment and cleanup quiesced the transport, released DMA mappings, reset
VFIO, verified the post-reset safe state, and restored the native driver with
`RESTORE end failed=0`. Native `mt7921e`, D0, iwd, WPA3 association, IPv4,
the default route, and gateway connectivity recovered in 84246 ms on the
renamed `wlan2`; the watchdog was disarmed without a reboot. The boot ID and
patched system, profile, kernel, generation-25 default,
`keep_d0_on_remove=Y`, and empty lab-state invariants remained intact. During
the slow recovery the kernel reported pool 22 with five inflight buffers at
60 seconds and three at 120 seconds, while native connectivity was healthy at
final collection. No rerun was attempted, and the physical deployment remains
held unchanged pending the H2E cryptographic and frame KAT audit.
