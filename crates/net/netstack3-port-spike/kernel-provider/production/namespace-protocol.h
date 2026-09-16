/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef NS3_NAMESPACE_PROTOCOL_H
#define NS3_NAMESPACE_PROTOCOL_H
#include <linux/ioctl.h>
#include <linux/types.h>
struct ns3_namespace_claim { __s32 provider_fd, monitor_fd; };
#define NS3_NAMESPACE_CLAIM _IOR(0xB5, 1, struct ns3_namespace_claim)
#define NS3_NAMESPACE_READY _IO(0xB5, 2)
#define NS3_NAMESPACE_CONTROL _IO(0xB5, 3)
#define NS3_NAMESPACE_REVOKE _IO(0xB5, 5)
#endif
