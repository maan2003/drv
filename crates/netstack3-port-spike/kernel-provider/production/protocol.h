/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _NS3_PROTOCOL_H
#define _NS3_PROTOCOL_H
#include <linux/types.h>
/* All scalar fields are little endian. Socket IDs are assigned by the kernel.
 * Each provider FD names exactly one network namespace and one generation.
 * Requests and replies have the same opcode, socket and request IDs.
 * RX/STATE events use request=0. No pointers or Linux object layouts cross.
 */
#define NS3_VERSION 3
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
#endif
