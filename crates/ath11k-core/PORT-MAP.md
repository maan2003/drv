# ath11k-core port map

Pinned oracle: Linux `509ce3d952d550f93b544c8d94c99e798f09a9b4`.

## Bring-up size metric

| Rust classification | production lines | share |
|---|---:|---:|
| reusable by another ath11k chip | 920 | 65.1% |
| WCN6750-specific | 319 | 22.6% |
| local composition seams | 175 | 12.4% |
| kernel substrate / replaced / deferred | 0 | not ported |
| total | 1,414 | 100% |

Tests (601 lines) are excluded. The pinned owned input is 21,925 physical
lines: `core.[ch]` 4,174; `ahb.[ch]` 1,362; `hw.[ch]` 3,318;
`peer.[ch]` 729; `reg.[ch]` 1,118; WCN6750 `hif.h` 162; and
`mac.[ch]` 11,062. Rows below classify the source spans reached by client
bring-up; unrelated chip tables are outside the WCN6750 port rather than
misreported as replaced code.

<!-- PORT-MAP-SCHEMA: C symbol | C file:lines | Rust item | status ∈ {ported, ported-corrected, wcn6750-specific, local-seam, replaced-by-fuchsia-mlme, kernel-substrate, deferred, blocked} | note -->
| C symbol | C file:lines | Rust item | status | note |
|---|---|---|---|---|
| `ath11k_hw_params[WCN6750]` | `core.c:578-662` | `hw::WCN6750_PARAMS` | wcn6750-specific | Complete scalar/feature table; CE/service maps remain CE-owned. |
| `ath11k_hw_ring_mask_wcn6750` | `hw.c:2036-2071` | `hw::WCN6750_RING_MASK` | wcn6750-specific | Source fixture asserted in core tests. |
| `wcn6750_regs` | `hw.c:2623-2708` | `ath11k_hal` WCN6750 register APIs | wcn6750-specific | HAL-owned checked registers/descriptors. |
| `ath11k_ahb_get_msi_irq_wcn6750` | `ahb.c:145-148` | `ahb::WCN6750_INTERRUPT_ROUTES` | wcn6750-specific | Typed source-active routes within the 10 CE vectors then 18 DP vectors. |
| `ath11k_ahb_config_irq` hybrid branch | `ahb.c:610-616` | `ahb::Wcn6750Interrupts::configure` | ported | WCN6750 delegates IRQ configuration to the PCIC MSI path. |
| `ath11k_pcic_get_ce_msi_idx` / CE configuration | `pcic.c:304-319,662-717` | `ahb::WCN6750_CE_INTERRUPT_ROUTES` / `Wcn6750Interrupts::configure_with_ce_polling` | ported-corrected | Skips disabled CE pipes and assigns active engines sequentially from CE MSI base 0; owned handles provide the CE completion wait. The VFIO bring-up path also polls the CE status rings every 10 ms while waiting because physical WCN6750 completions can update a ring without waking its eventfd. |
| `ath11k_pcic_ext_irq_config` | `pcic.c:573-660` | `ahb::WCN6750_DP_INTERRUPT_ROUTES` / `Wcn6750DpInterrupts::enable` | ported | Nonempty WCN6750 ring-mask groups map to DP base vector 10 plus group; opening all routes is transactional. |
| `ath11k_pcic_ext_irq_enable` / `ath11k_pcic_ext_irq_disable` | `pcic.c:435-521` | `Wcn6750DpInterrupts::{enable,disable}` | ported-corrected | The portable backend has no kernel IRQ masking primitive, so enable opens routes and disable drops them physically; no NAPI policy crosses the seam. |
| `ath11k_ahb_get_window_start_wcn6750` | `ahb.c:151-164` | `ahb::RegisterWindow::for_offset` | wcn6750-specific | Static DP/CE window selection. |
| `ath11k_ahb_window_write32_wcn6750` | `ahb.c:167-176` | `ahb::wcn6750_register_offset` / `MmioRegion::map_offsets` | wcn6750-specific | The QMI-discovered 2 MiB aperture translates every bounded CE/HAL/DP write. |
| `ath11k_ahb_window_read32_wcn6750` | `ahb.c:178-189` | `ahb::wcn6750_register_offset` / `MmioRegion::map_offsets` | wcn6750-specific | The QMI-discovered 2 MiB aperture translates every bounded CE/HAL/DP read. |
| `ath11k_ahb_start` | `ahb.c:365-371` | `Operation::HifStart` / CE `rx_post_buf` | ported | Normative lifecycle transcript plus real CE API. |
| `ath11k_ahb_stop` | `ahb.c:394-402` | `Operation::HifStop` | ported | Crash-flush distinction retained. |
| `ath11k_pcic_ce_interrupt_handler` / `ath11k_pcic_ce_tasklet` | `pcic.c:406-432` | `Wcn6750CeInterrupts::wait_any` / `ahb::service_ce_interrupt` | ported | WCN6750 hybrid-bus delivery is typed by CE engine and directly dispatches real CE service. |
| `ath11k_pcic_ext_interrupt_handler` / `ath11k_pcic_ext_grp_napi_poll` | `pcic.c:523-565` | `Wcn6750DpInterrupts::wait_any` / `ahb::service_dp_external_group` | ported | Returns every ready typed external group; directly dispatches real budgeted DP service while leaving NAPI policy out of the userspace core. |
| `ath11k_ahb_power_up` | `ahb.c:404-414` | — | kernel-substrate | remoteproc/SMEM/SMP2P power is complete before VFIO handoff. |
| `ath11k_ahb_power_down` | `ahb.c:416-421` | — | kernel-substrate | Host kernel retains substrate teardown. |
| `ath11k_ahb_probe` substrate resource portion | `ahb.c:1108-1165` | `ath11k_platform_backend` | kernel-substrate | Platform device, SMMU and IRQ delivery are host-owned. |
| `ath11k_ahb_probe` post-substrate portion | `ahb.c:1166-1231` | `Wcn6750::device` / `Lifecycle::probe` | ported | Userspace starts after resources/substrate exist. |
| `ath11k_core_soc_create` | `core.c:1985-2019` | `Device::probe` | ported | QMI init then HIF power-up ordering. |
| `ath11k_core_soc_destroy` | `core.c:2021-2027` | `Device::stop` | ported | Reverse-order composition. |
| `ath11k_core_start` | `core.c:2133-2242` | `Device::core_start` | ported | Exact WMI/HTC/HIF/HTT/DP ordering and fall-through unwind asserted. |
| `ath11k_core_stop` | `core.c:1973-1983` | `Device::core_stop` | ported | Crash flush suppresses QMI firmware-stop only. |
| `ath11k_core_start_firmware` | `core.c:2244-2259` | `Wcn6750QmiSession::firmware_start` | ported | Real event-driven QMI session. |
| `ath11k_core_qmi_firmware_ready` | `core.c:2261-2327` | `Device::attach_firmware` / `Wcn6750Subsystems::wait_for_firmware_ready` | ported | The validated DEVICE_INFO aperture transfers from QMI to HIF before CE/DP register access; CE/DP/core/pdev/IRQ order is asserted. |
| `ath11k_core_pdev_create` | `core.c:2029-2084` | `DpPdevAllocate` / `MacRegister` operations | ported | Thermal/spectral/CFR branches classified separately. |
| `ath11k_core_pdev_destroy` | `core.c:2121-2131` | `Device::stop` pdev phase | ported | Includes WCN6750 pdev suspend before IRQ disable. |
| `ath11k_core_pre_reconfigure_recovery` | `core.c:2417-2464` | `Device::firmware_crashed` | ported | Quiesces outstanding waits without panic. |
| `ath11k_core_post_reconfigure_recovery` | `core.c:2466-2509` | `Operation::RecoveryRestart` | ported | Restart/wedge transition retained. |
| `ath11k_core_restart` | `core.c:2511-2529` | `Device::complete_recovery` | ported | Re-enters firmware-ready lifecycle. |
| `ath11k_core_init` | `core.c:2686-2717` | `Lifecycle::probe` | ported | PM notifier portion remains kernel-owned. |
| `ath11k_core_deinit` | `core.c:2720-2733` | `Lifecycle::stop` | ported | Deterministic teardown asserted. |
| QMI firmware assets | `core.c:735-1007` | `qmi::Wcn6750FirmwareAssets` | local-seam | Assets are supplied before filesystem capability is dropped. |
| QMI target memory | `qmi.c:1955-2037` | `qmi::HardwareMemoryProvider` | local-seam | Real generation-tied DMA and allocation-derived IOVAs. |
| QMI event loop | `qmi.c:3025-3137` | `qmi::Wcn6750QmiSession` | ported | Actual QMI crate tested through fake QRTR peer; firmware-ready is returned by the event loop to core attach. |
| `ath11k_peer_create` | `peer.c:370-472` | `RadioControl::create_peer` | ported | WMI command plus correlated creation wait. |
| `ath11k_peer_delete` | `peer.c:350-363` | `RadioControl::delete_peer` | ported | WMI command plus correlated deletion wait. |
| `ath11k_mac_op_add_interface` STA effects | `mac.c:7073-7331` | `RadioControl::create_client_vdev` | ported | Vdev create, NSS, PS, RTS, DP attach and unwind order asserted. |
| `ath11k_mac_vdev_start` | `mac.c:7716-7720` | `RadioControl::start_vdev` | ported | Typed channel and setup completion. |
| `ath11k_mac_vdev_stop` | `mac.c:7676-7714` | `ClientRadioControl::stop_vdev` | ported | Command then setup completion. |
| `ath11k_bss_assoc` | `mac.c:3093-3209` | `PeerAssociation` / `associate_peer` / `up_vdev` | ported | Exact open Cbw20 legacy/HT/VHT parameters, optional WMM, correlated completion, SMPS, vdev-up, OBSS and DTIM order. Secure association is blocked because pinned FIDL omits RSN/WPA/PMF facts; wider widths require widening vdev start. |
| `ath11k_bss_disassoc` | `mac.c:3211-3233` | `ClientRadioControl::down_vdev` | ported | Policy-free hardware effect. |
| `ath11k_install_key` / `ath11k_mac_op_set_key` | `mac.c:4352-4417,4492-4640` | `KeyConfig` / `ClientRadioControl::install_key` / `Operation::DpInstallPeerKey` | ported-corrected | Core accepts RxTx protection only; WMI mapping preserves the 48-bit RSC and cipher and waits for correlated completion before pairwise REO replay configuration and peer key/security publication. Exact completed retries are idempotent; uncertain DP publication is terminal at the adapter boundary. As in pinned Linux, BIP/IGTK is rejected by this firmware-key seam and implemented in software by the SoftMAC adapter. |
| `ath11k_mac_op_hw_scan` | `mac.c:4183-4338` | `ClientRadioControl::start_scan` | ported | Typed request/channel/SSID data. |
| `ath11k_mac_op_cancel_hw_scan` | `mac.c:4340-4350` | `ClientRadioControl::stop_scan` | ported | WMI scan stop effect. |
| `ath11k_mac_mgmt_tx_wmi` | `mac.c:6174-6254` | `ClientRadioControl::transmit_management` | ported | Buffer identity retained for completion. |
| `ath11k_reg_set_cc` | `reg.c:1062-1068` | `ClientRadioControl::set_regulatory_domain` | ported | Current-country command is followed by its fresh regulatory event before the channel list is installed. |
| `ath11k_reg_update_chan_list` | `reg.c:118-222` | `RegulatoryDomain` / `RegulatoryChannel` | ported | Empty-list check is the native check. |
| WMI/HTC receive events | `wmi.c:6507-7950` | `events::WlanEvent` | ported | Actual typed decoders; only roam reason 2 becomes beacon loss. |
| mac80211 scan/association/rate-control policy | `mac.c:1-10878` | Fuchsia wlan-mlme/SME | replaced-by-fuchsia-mlme | Hardware-effect spans above are excluded from this classification. |
| cfg80211 regulatory rule policy | `reg.c:224-1060` | Fuchsia regulatory/MLME policy | replaced-by-fuchsia-mlme | WMI channel effects remain ported. |
| debugfs calls | `core.c:1999-2007` | — | deferred | Explicitly excluded. |
| thermal/spectral/CFR pdev setup | `core.c:2050-2075` | — | deferred | Explicitly excluded. |
| coredump/testmode recovery extras | `core.c:2531-2684` | — | deferred | Explicitly excluded. |
| simultaneous WMI + HTT endpoint handles | `htc.c:150-244` | `HtcRouter` / `HtcWmiTransport` / `HtcHttTransport` | ported | One router owns HTC credit state and demultiplexes independent endpoint handles; core exercises both real adapters together. |
| core operation transcript model | — | `operation::ModelSubsystems` | local-seam | Retained as ordering oracle, not used as the real transport test. |
