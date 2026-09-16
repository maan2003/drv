/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef NS3_NAMESPACE_PROTOCOL_H
#define NS3_NAMESPACE_PROTOCOL_H
#include <linux/ioctl.h>
#include <linux/types.h>
struct ns3_namespace_claim { __s32 provider_fd, monitor_fd; };
struct ns3_namespace_state { __u64 revision; __u32 flags, reserved; };
#define NS3_NAMESPACE_CLAIM _IOR(0xB5, 1, struct ns3_namespace_claim)
#define NS3_NAMESPACE_STATE _IOR(0xB5, 2, struct ns3_namespace_state)
#define NS3_NAMESPACE_ACK _IOW(0xB5, 3, __u64)
#define NS3_NAMESPACE_SET_UP _IOW(0xB5, 4, __u32)
#define NS3_NAMESPACE_REVOKE _IO(0xB5, 5)
#endif
