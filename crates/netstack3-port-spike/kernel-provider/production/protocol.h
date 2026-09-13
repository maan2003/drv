/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _NS3_PROTOCOL_H
#define _NS3_PROTOCOL_H
#include <linux/types.h>
/* All scalar fields are little endian. Socket IDs are assigned by the kernel.
 * The registration FD scopes a namespace/generation; claimed FDs each name one socket.
 * Requests and replies have the same opcode, socket and request IDs.
 * SEND completion means ownership transferred to core, not remote delivery.
 * SHUTDOWN(write) and CLOSE are ordered producer barriers: the provider must
 * transfer all earlier admitted stream bytes, including a bounded remainder
 * exceeding ordinary TCP send-buffer space, before shutting down core. Neither
 * barrier waits for peer ACKs. Admission stops under the kernel transmit lock;
 * outstanding SEND bytes remain charged until their individual completions.
 * Read credits run independently of these barriers. EOF follows buffered RX.
 * RX/STATE and allocation-free CREDIT/CLOSE/ACCEPT-space notifications use request=0. No pointers or Linux object layouts cross.
 */
/* ABI5 moves the write-shutdown drain barrier from kernel to core owner.
 * An ABI4 worker can discard queued bytes with this kernel: fail closed. */
#define NS3_VERSION 5
/* _IOR(0xB3, 1, __u64): returns a new O_CLOEXEC endpoint FD, writes socket ID. */
#define NS3_CLAIM 0x8008B301
/* _IOWR(0xB3, 2, ns3_accept_info): publish an accepted child; return its FD. */
#define NS3_PUBLISH_ACCEPT 0xC038B302
#define NS3_PAYLOAD 16384
#define NS3_OPEN 1
#define NS3_BIND 2
#define NS3_LISTEN 3
#define NS3_CONNECT 4
#define NS3_ACCEPT 5
#define NS3_SEND 6
#define NS3_SHUTDOWN 7
#define NS3_CLOSE 8
#define NS3_RX 9
#define NS3_STATE 10
#define NS3_GETNAME 11
#define NS3_SETOPT 12
#define NS3_CREDIT 13
#define NS3_CONNECTED 1
#define NS3_LISTENING 2
#define NS3_EOF 4
struct ns3_msg {
	__le32 version;
	__le32 op;
	__le64 socket;
	__le64 request;
	__le32 len;
	__le32 status; /* response: positive Linux errno, zero success */
};
struct ns3_addr {
	__le16 family; /* 4 or 6 */
	__le16 port;
	__u8 address[16];
	__le32 scope;
};
struct ns3_accept_info {
	struct ns3_addr local, peer;
	__le64 socket;
};
#endif
