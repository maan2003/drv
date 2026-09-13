// SPDX-License-Identifier: GPL-2.0-only
/*
 * Experimental Linux integration only, not a TCP/UDP provider.
 * Rust owns all admission, generation, readiness and socket registry state.
 * C owns registration, net references and opaque foreign owners. Rust also
 * owns poll registration and wait-queue teardown through upstream wrappers.
 * This is intentionally not a mechanical Rust proto_ops port.
 */
#include <linux/module.h>
#include <linux/net.h>
#include <linux/in.h>
#include <linux/miscdevice.h>
#include <linux/poll.h>
#include <linux/slab.h>
#include <linux/nsproxy.h>
#include <net/sock.h>
#include <net/net_namespace.h>
#include <net/netns/generic.h>
#include "protocol.h"

/* Rust allocation uses kernel allocators. All borrows and drops below are
 * sleepable. Opaque owners are non-NULL on success; copied pointers are borrows,
 * never additional owners. Rust Arc retains state independently of C netns. */
extern void *nsrl_namespace_new(void);
extern void nsrl_namespace_drop(void *);
extern int nsrl_session_new(void *, void **);
extern void nsrl_session_drop(void *);
extern int nsrl_set_ready(void *, u64, bool);
extern int nsrl_socket_new(void *, void **);
extern void nsrl_socket_drop(void *);
extern u64 nsrl_socket_id(void *);
extern u32 nsrl_socket_poll(void *, const struct file *, poll_table *);
extern size_t nsrl_live_sockets(void);
extern size_t nsrl_live_namespaces(void);

struct nsrl_net {
	void *state;
};
struct nsrl_sock {
	struct sock sk;
	void *state;
};
struct nsrl_session {
	struct net *net;
	void *state;
};
static unsigned int nsrl_net_id;
static const struct proto_ops nsrl_ops4, nsrl_ops6;
static struct proto nsrl_proto = {
	.name = "NSRL_EXPERIMENT", .owner = THIS_MODULE, .obj_size = sizeof(struct nsrl_sock),
};
static struct nsrl_net *nsrl_net(struct net *net)
{
	return net_generic(net, nsrl_net_id);
}
static struct nsrl_sock *nsrl_sock(struct socket *sock)
{
	return container_of(sock->sk, struct nsrl_sock, sk);
}

static int nsrl_release(struct socket *sock)
{
	struct nsrl_sock *s;
	if (!sock->sk) return 0;
	s = nsrl_sock(sock);
	/* VFS final release excludes poll/ioctl and runs in sleepable context.
	 * Drop here, NOT sk_destruct (which may run in atomic/RCU context). */
	nsrl_socket_drop(s->state);
	s->state = NULL;
	sock_orphan(&s->sk);
	sock->sk = NULL;
	sock_put(&s->sk);
	return 0;
}
static __poll_t nsrl_poll(struct file *file, struct socket *sock, poll_table *wait)
{
	struct nsrl_sock *s = nsrl_sock(sock);
	switch (nsrl_socket_poll(s->state, file, wait)) {
	case 0: return 0;
	case 1: return EPOLLIN | EPOLLRDNORM | EPOLLOUT | EPOLLWRNORM;
	default: return EPOLLERR | EPOLLHUP;
	}
}
static int nsrl_ioctl(struct socket *sock, unsigned int cmd, unsigned long arg)
{
	u64 id;
	if (cmd != NSRL_ID) return -ENOIOCTLCMD;
	id = nsrl_socket_id(nsrl_sock(sock)->state);
	return copy_to_user((void __user *)arg, &id, sizeof(id)) ? -EFAULT : 0;
}
static int nsrl_create(struct net *net, struct socket *sock, int protocol, int kern, int family)
{
	struct sock *sk;
	struct nsrl_sock *s;
	int ret;
	if (kern) return -EOPNOTSUPP;
	if ((sock->type != SOCK_STREAM || (protocol && protocol != IPPROTO_TCP)) &&
	    (sock->type != SOCK_DGRAM || (protocol && protocol != IPPROTO_UDP)))
		return -EPROTONOSUPPORT;
	sk = sk_alloc(net, family, GFP_KERNEL, &nsrl_proto, false);
	if (!sk) return -ENOMEM;
	sock_init_data(sock, sk);
	s = nsrl_sock(sock);
	ret = nsrl_socket_new(nsrl_net(net)->state, &s->state);
	if (ret) {
		sock_orphan(sk); sock->sk = NULL; sock_put(sk); return ret;
	}
	/* After this point, the socket file owns the foreign Socket exactly once. */
	sock->ops = family == PF_INET ? &nsrl_ops4 : &nsrl_ops6;
	sock->state = SS_UNCONNECTED;
	return 0;
}
static int nsrl_create4(struct net *n, struct socket *s, int p, int k)
{
	return nsrl_create(n, s, p, k, PF_INET);
}
static int nsrl_create6(struct net *n, struct socket *s, int p, int k)
{
	return nsrl_create(n, s, p, k, PF_INET6);
}
#define NSRL_OPS(fam) { .family = fam, .owner = THIS_MODULE, \
	.release = nsrl_release, .poll = nsrl_poll, .ioctl = nsrl_ioctl, \
	.bind = sock_no_bind, .connect = sock_no_connect, .socketpair = sock_no_socketpair, \
	.accept = sock_no_accept, .getname = sock_no_getname, .listen = sock_no_listen, \
	.shutdown = sock_no_shutdown, .sendmsg = sock_no_sendmsg, .recvmsg = sock_no_recvmsg, \
	.mmap = sock_no_mmap }
static const struct proto_ops nsrl_ops4 = NSRL_OPS(PF_INET);
static const struct proto_ops nsrl_ops6 = NSRL_OPS(PF_INET6);
static const struct net_proto_family nsrl_family4 = {
	.family = PF_INET, .create = nsrl_create4, .owner = THIS_MODULE,
};
static const struct net_proto_family nsrl_family6 = {
	.family = PF_INET6, .create = nsrl_create6, .owner = THIS_MODULE,
};

static int nsrl_open(struct inode *inode, struct file *file)
{
	struct nsrl_session *s;
	int ret;
	if (!ns_capable(current->nsproxy->net_ns->user_ns, CAP_NET_ADMIN)) return -EPERM;
	s = kzalloc(sizeof(*s), GFP_KERNEL);
	if (!s) return -ENOMEM;
	s->net = get_net(current->nsproxy->net_ns);
	ret = nsrl_session_new(nsrl_net(s->net)->state, &s->state);
	if (ret) { put_net(s->net); kfree(s); return ret; }
	file->private_data = s;
	return 0;
}
static int nsrl_provider_release(struct inode *inode, struct file *file)
{
	struct nsrl_session *s = file->private_data;
	nsrl_session_drop(s->state);
	put_net(s->net);
	kfree(s);
	return 0;
}
static long nsrl_provider_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	struct nsrl_session *s = file->private_data;
	struct nsrl_ready update;
	struct nsrl_stats stats;
	int ret;
	if (cmd == NSRL_STATS) {
		stats.sockets = nsrl_live_sockets();
		stats.namespaces = nsrl_live_namespaces();
		return copy_to_user((void __user *)arg, &stats, sizeof(stats)) ? -EFAULT : 0;
	}
	if (cmd != NSRL_READY) return -ENOTTY;
	if (copy_from_user(&update, (void __user *)arg, sizeof(update))) return -EFAULT;
	if (update.ready > 1 || update.reserved) return -EINVAL;
	ret = nsrl_set_ready(s->state, update.id, update.ready);
	return ret;
}
static const struct file_operations nsrl_fops = {
	.owner = THIS_MODULE, .open = nsrl_open, .release = nsrl_provider_release,
	.unlocked_ioctl = nsrl_provider_ioctl,
};
static struct miscdevice nsrl_device = {
	.minor = MISC_DYNAMIC_MINOR, .name = "ns3-rust-lifecycle", .mode = 0600,
	.fops = &nsrl_fops,
};
static int __net_init nsrl_net_init(struct net *net)
{
	struct nsrl_net *n = nsrl_net(net);
	n->state = nsrl_namespace_new();
	return n->state ? 0 : -ENOMEM;
}
static void __net_exit nsrl_net_exit(struct net *net)
{
	/* All socket/provider net references are gone. Per-net exit is sleepable. */
	nsrl_namespace_drop(nsrl_net(net)->state);
}
static struct pernet_operations nsrl_pernet = {
	.init = nsrl_net_init, .exit = nsrl_net_exit,
	.id = &nsrl_net_id, .size = sizeof(struct nsrl_net),
};
static int __init nsrl_init(void)
{
	int ret = proto_register(&nsrl_proto, 1);
	if (ret) return ret;
	ret = register_pernet_subsys(&nsrl_pernet);
	if (ret) goto proto;
	ret = sock_register(&nsrl_family4);
	if (ret) goto pernet;
	ret = sock_register(&nsrl_family6);
	if (ret) goto family4;
	ret = misc_register(&nsrl_device);
	if (!ret) {
		pr_info("NSRL_EXPERIMENT Rust lifecycle active; no data path\n");
		return 0;
	}
	sock_unregister(PF_INET6);
family4:
	sock_unregister(PF_INET);
pernet:
	unregister_pernet_subsys(&nsrl_pernet);
proto:
	proto_unregister(&nsrl_proto);
	return ret;
}
subsys_initcall(nsrl_init);
MODULE_LICENSE("GPL");
