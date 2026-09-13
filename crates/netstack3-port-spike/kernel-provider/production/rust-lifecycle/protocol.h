/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef NSRL_PROTOCOL_H
#define NSRL_PROTOCOL_H
#include <linux/types.h>
#include <linux/ioctl.h>
/* Test-only control protocol, not production ABI5. IDs scope to the opening
 * provider's net namespace and generation; no kernel pointers cross. */
struct nsrl_ready { __u64 id; __u32 ready; __u32 reserved; };
struct nsrl_stats { __u64 sockets; __u64 namespaces; };
#define NSRL_ID _IOR(0xB4, 1, __u64)
#define NSRL_READY _IOW(0xB4, 2, struct nsrl_ready)
#define NSRL_STATS _IOR(0xB4, 3, struct nsrl_stats)
#endif
