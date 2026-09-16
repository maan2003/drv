/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef _NS3_PROTOCOL_H
#define _NS3_PROTOCOL_H
#include <linux/types.h>
/*
 * ABI6: all scalars LE. Registration scopes namespace/generation. Each claimed
 * endpoint FD is the sole socket capability; ordinary records contain no ID.
 * CLAIM/PUBLISH alone carry IDs for registration and epoll bookkeeping.
 *
 * read(endpoint): dequeue TX stream bytes (partial permitted) or one atomic
 * UDP record. write(endpoint): RX admission or typed control/state outcome.
 * READ_CONTROL ioctl: independent bounded control lane; 128-byte output area,
 * returns actual record length. Empty TX/control/full RX => EAGAIN.
 * User-copy failure commits no queue/dispatch ownership transition.
 *
 * Data uses request=0 and has no reply. Kernel admission is app send success,
 * dequeue is binding ownership, core write is core ownership; none means peer
 * delivery. UDP destination is fixed at kernel admission, not pump time.
 * Kernel queues bound bytes AND records, including zero-length datagrams.
 * Endpoint POLLOUT guarantees capacity for one maximum-sized RX record.
 * No mirrored RX credits. A blocked RX binding retains one bounded record.
 *
 * OPEN(kind:u32,family:u32) -> local address (unbound).
 * BIND(address), LISTEN(backlog:u32), ACTIVATE(empty) -> committed local.
 * ACTIVATE lazily binds UDP before first send admission. Nonblocking callers
 * get EAGAIN with no payload consumed; preparation continues, poll wakes.
 * CONNECT(peer) -> local+peer acknowledgement; TCP then requires CONNECTION
 * outcome with that attempt ID (local+peer or errno). Attempt survives SO_ERROR.
 * GETNAME is local committed metadata, not an RPC.
 * SHUTDOWN(how:u32,seal:u64) -> empty terminal outcome.
 * CLOSE(seal:u64), ACCEPT-space(empty) are allocation-free notifications.
 * Seal = cumulative admitted TCP bytes / UDP records (zero datagrams count).
 * Producer admission stops atomically with write seal; provider drains through
 * seal including staging, finishes bounded core handoff, then shuts down/closes.
 * Control priority cannot retarget data. EOF STATE follows all preceding RX.
 * Endpoint/provider failure aborts, never waits for graceful drain.
 *
 * Every outcome is matched and shape/family validated before commit/retirement/
 * wake. Dispatched mutating control interrupted by a waiter revokes on ambiguity;
 * undispatched cancellation withdraws. Background preparation is not cancellation.
 * Successful terminal handoff does not wait for peer ACK. Runtime storage leases
 * cover core+terminal allowance after handle close; kernel/staging/copy overlap
 * are separate bounded populations. No pointers/native layouts cross the ABI.
 */
#define NS3_VERSION 6
#define NS3_CLAIM 0x8008B301
#define NS3_PUBLISH_ACCEPT 0xC038B302
#define NS3_READ_CONTROL 0x8080B303
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
#define NS3_ACTIVATE 13
#define NS3_CONNECTION 14
#define NS3_EOF 4
struct ns3_msg {
    __le32 version, op;
    __le64 request;
    __le32 len, status; /* positive Linux errno, zero success */
};
struct ns3_addr {
    __le16 family, port; /* family 4 or 6 */
    __u8 address[16];
    __le32 scope; /* currently zero */
};
struct ns3_accept_info {
    struct ns3_addr local, peer;
    __le64 socket;
};
#endif
