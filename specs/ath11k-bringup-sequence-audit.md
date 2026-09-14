# ath11k WCN6750 bring-up sequence audit

Source authority is Linux commit
`509ce3d952d550f93b544c8d94c99e798f09a9b4`. The decomposition and exact
public port seams are in `crates/wifi/drivers/ath11k/ath11k-core/INTERFACES.md`.

| phase | minimum Linux ownership path | completion evidence |
|---|---|---|
| WPSS ready | kernel remoteproc/PIL authenticates and boots `wpss`; QRTR service appears | remoteproc running and QMI service reachable |
| QMI handshake | server arrival → indication registration → host capability → target capability → hybrid DeviceInfo/BAR → regdb/BDF on the fixed-memory path; memory responses and firmware-ready progress follow the applicable firmware indications | DeviceInfo proves BAR discovery only; full startup additionally requires the applicable firmware-ready completion |
| transport | AHB/HIF powers CE; HTC connects WMI control and HTT data services | service-ready/credit messages |
| WMI init | service-ready/unified-ready, resource config, init, pdev capability/regulatory setup | WMI ready event and pdev created |
| scan | vdev create/start plus scan-start TLVs; scan events deliver BSS frames | scan completion and selected BSS |
| association | peer create/assoc, vdev up, key install and controlled-port effects driven through WlanSoftmac | WMI peer/vdev/key completions plus MLME association |
| data | HTT SRNG setup; TCL publishes TX, REO consumes RX, WBM returns buffers/completions; CE carries HTT control | ring ownership moves and traffic passes |

The ordering boundary is firmware readiness, not module probe: PIL and generic
Qualcomm IPC stay kernel-owned. QMI is independent of WMI. WMI and HTT are HTC
services carried over CE; CE publishes through HAL-owned rings. DP directly
owns TCL/REO/WBM policy but uses HAL descriptor and SRNG codecs. Core composes
these and owns pdev/vdev/peer lifetimes.

`mac.c` is not ported wholesale. Its `ieee80211_ops`, cfg80211/mac80211
adaptation, scan/association decisions and management-frame policy are replaced
by Fuchsia MLME. Its hardware effects—channel/pdev parameters, vdev
create/start/up/down/delete, peer create/assoc/delete, key installation and
data-path metadata—remain and are exposed behind the narrow `RadioControl`
implementation.

Native tracepoint observability is asymmetric. WMI command/event tracepoints
carry exact dynamic byte arrays. HTT exposes pktlog, PPDU statistics and
selected RX descriptors, not every host/target control message. QMI payloads,
CE descriptors, TCL TX descriptors, general REO/WBM ring entries and MMIO
writes are not exposed by current tracepoints. Those require temporary narrow
instrumentation or supervised DMA snapshots; hardware behavior cannot be
inferred from the textual ftrace formatter.
