# Linux QRTR UAPI port map

This crate is a clean Rust binding to the stable userspace ABI in
`include/uapi/linux/qrtr.h`. It does not copy or link GPL-only QRTR libraries.

| Linux UAPI symbol | Rust owner | Representation |
|---|---|---|
| `AF_QIPCRTR` (`42`) | private `AF_QIPCRTR` | used only by `QrtrSocket::open` |
| `struct sockaddr_qrtr` | private `SockAddrQrtr` | `repr(C)` syscall value converted to/from `QrtrAddr` |
| `QRTR_PORT_CTRL` (`0xfffffffe`) | `CONTROL_PORT` | lookup destination and control-source discriminator |
| `QRTR_TYPE_BYE` (`3`) | private `QRTR_TYPE_BYE` | parsed as `ControlEvent::Bye` |
| `QRTR_TYPE_NEW_SERVER` (`4`) | private `QRTR_TYPE_NEW_SERVER` | parsed as `ControlEvent::ServerAdded` |
| `QRTR_TYPE_DEL_SERVER` (`5`) | private `QRTR_TYPE_DEL_SERVER` | parsed as `ControlEvent::ServerRemoved` |
| `QRTR_TYPE_NEW_LOOKUP` (`10`) | private `QRTR_TYPE_NEW_LOOKUP` | emitted by `subscribe` and `lookup` |
| `struct qrtr_ctrl_pkt` | `encode_lookup`, `ControlEvent::parse` | exactly 20 hand-coded little-endian bytes; no layout cast or transmute |

The C socket address fields use native ABI integer representation. All five
control-packet words are explicitly little endian, matching their UAPI
`__le32` declarations.
