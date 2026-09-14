<!-- PORT-MAP-SCHEMA: C symbol | C file:lines | Rust item | status ∈ {ported, ported-corrected, wcn6750-specific, local-seam, replaced-by-fuchsia-mlme, kernel-substrate, deferred, blocked} | note -->
| C symbol | C file:lines | Rust item | status | note |
|---|---|---|---|---|
| `struct qmi_header` | `include/linux/soc/qcom/qmi.h:19-30` | `src/lib.rs::encode_request` / `decode_qmi_packet` | ported | Exact packed 7-byte QMI envelope fixture covered. |
| `QMI_REQUEST` | `include/linux/soc/qcom/qmi.h:32-32` | `src/lib.rs::QMI_REQUEST` | ported | Request envelope type 0. |
| `QMI_RESPONSE` | `include/linux/soc/qcom/qmi.h:33-33` | `src/lib.rs::QMI_RESPONSE` | ported | Response envelope type 2 with transaction correlation. |
| `QMI_INDICATION` | `include/linux/soc/qcom/qmi.h:34-34` | `src/lib.rs::QMI_INDICATION` | ported | Unsolicited indication envelope type 4. |
| `QMI_SERVICE_ID_WLFW` | `include/linux/soc/qcom/qmi.h:103-103` | `src/lib.rs::QMI_SERVICE_ID_WLFW` | ported | WLFW service 0x45; WCN6750 packed instance is 0x301. |
| `qmi_add_lookup` | `drivers/soc/qcom/qmi_interface.c:207-225` | `src/lib.rs::QrtrTransport::start_service` | ported | Persistent service lookup yields `Incoming::ServerArrived`. |
| `qmi_handle_message` | `drivers/soc/qcom/qmi_interface.c:472-513` | `src/lib.rs::QrtrTransport::receive` | ported | Separates responses/indications and retains transaction IDs. |
| `qmi_handle_release` | `drivers/soc/qcom/qmi_interface.c:687-720` | `src/lib.rs::QrtrTransport::stop_service` | ported | Drops the owned socket, removing kernel lookup and closing the fd. |
| `AF_QIPCRTR` syscall implementation | `include/uapi/linux/qrtr.h:11-47` | `qrtr-socket::QrtrSocket` | kernel-substrate | Unsafe ABI is confined below this `forbid(unsafe_code)` adapter. |
