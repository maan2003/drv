/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef NS3_NETLINK_PROTOCOL_H
#define NS3_NETLINK_PROTOCOL_H
#include <linux/types.h>
/*
 * /dev/netstack3-netlink registers NETLINK_ROUTE in the opener's namespace.
 * Privileged launcher passes this FD into the sandbox. Registration applies
 * to subsequently created user route sockets. Existing sockets retain their
 * original owner. Once delegated, absence never falls back to kernel state.
 * CLAIM returns a nonblocking, CLOEXEC FD permanently bound to one socket.
 *
 * Scalars below are LE. Netlink payloads retain their native Linux wire ABI.
 * read(endpoint) returns one atomic header + request record.
 * Copy failure/short read leaves the record pending. ENETDOWN retires endpoint.
 * Requests carry kernel-authenticated context, never nlmsg_pid as authority.
 *
 * write(registration): nonzero group:u32,reserved-zero:u32,raw datagram.
 * Multicast targets this generation's current subscribers, even unclaimed
 * sockets. Slow recipients get native overrun handling (ENOBUFS unless they\n * opted out with NETLINK_NO_ENOBUFS), never a global stall.
 * Native subscription bookkeeping stays in Linux; it is not mirrored remotely.
 *
 * write(endpoint): group:u32,reserved-zero:u32,raw-netlink-datagram.
 * group 0 replies to this exact socket (sender port ID 0); nonzero delivers
 * only if this socket is subscribed. No destination socket ID is accepted.
 * EAGAIN means retry after writable; ENOENT means membership was withdrawn.
 * Application RX uses native socket memory limits. TX bounds: 64KiB payload,
 * 32 records / 256KiB per socket; namespace quota 256, including retained FDs.
 * Registration death wakes/revokes old endpoints; replacement cannot revive.
 */
#define NS3_NL_CLAIM 0x8008B401
#define NS3_NL_VERSION 1
#define NS3_NL_REQUEST 1
#define NS3_NL_MAX_MESSAGE 65536
struct ns3_nl_record {
    __le32 version, kind, len, reserved;
    __le32 portid, uid, gid, pid, net_admin, context_reserved;
};
#endif
