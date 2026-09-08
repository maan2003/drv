<!-- PORT-MAP-SCHEMA: C symbol | C file:lines | Rust item | status ∈ {ported, ported-corrected, wcn6750-specific, local-seam, replaced-by-fuchsia-mlme, kernel-substrate, deferred, blocked} | note -->
| C symbol | C file:lines | Rust item | status | note |
|---|---|---|---|---|
| `AF_QIPCRTR` | `include/linux/socket.h:248-248` | `src/lib.rs::AF_QIPCRTR` | kernel-substrate | Linux address-family ABI; used only by the audited leaf constructor. |
| `QRTR_PORT_CTRL` | `include/uapi/linux/qrtr.h:9-9` | `src/lib.rs::CONTROL_PORT` | ported | Name-service port. |
| `struct sockaddr_qrtr` | `include/uapi/linux/qrtr.h:11-15` | `src/lib.rs::SockAddrQrtr` | kernel-substrate | Private initialized `repr(C)` syscall value converted to safe `QrtrAddr`. |
| `QRTR_TYPE_BYE` | `include/uapi/linux/qrtr.h:20-20` | `src/lib.rs::ControlEvent::Bye` | ported | Little-endian control parser fixture covered. |
| `QRTR_TYPE_NEW_SERVER` | `include/uapi/linux/qrtr.h:21-21` | `src/lib.rs::ControlEvent::ServerAdded` | ported | Includes the all-zero lookup-complete sentinel. |
| `QRTR_TYPE_DEL_SERVER` | `include/uapi/linux/qrtr.h:22-22` | `src/lib.rs::ControlEvent::ServerRemoved` | ported | Persistent lookup removal notification. |
| `QRTR_TYPE_NEW_LOOKUP` | `include/uapi/linux/qrtr.h:27-27` | `src/lib.rs::QrtrSocket::subscribe` | ported | Exact 20-byte little-endian request fixture covered. |
| `struct qrtr_ctrl_pkt` | `include/uapi/linux/qrtr.h:31-47` | `src/lib.rs::ControlEvent::parse` | ported | Hand-coded bytes; no layout cast/transmute. |
| `socket` / `bind` / `connect` / `sendto` / `recvfrom` | `include/uapi/linux/qrtr.h:11-15` | `src/lib.rs::QrtrSocket` | kernel-substrate | Only authorized unsafe leaf; every syscall block documents pointer/layout/fd invariants. |
