// SPDX-License-Identifier: GPL-2.0-only
#include <linux/module.h>
#include <linux/net.h>
#include <linux/in.h>
#include <linux/miscdevice.h>
#include <linux/poll.h>
#include <linux/anon_inodes.h>
#include <linux/nsproxy.h>
#include <linux/netdevice.h>
#include <linux/compat.h>
#include <linux/slab.h>
#include <linux/uaccess.h>
#include <linux/sockios.h>
#include <linux/user_namespace.h>
#include <net/sock.h>
#include <net/net_namespace.h>
#include <net/netns/generic.h>

struct rust_net { void *state; };
struct rust_sock { struct sock sk; void *state; };
static unsigned int net_id;
static const struct proto_ops ops4, ops6;
static struct proto proto = { .name = "NETSTACK3_RUST", .owner = THIS_MODULE,
	.obj_size = sizeof(struct rust_sock) };
extern void *ns3_net_new(void);
extern void ns3_net_drop(void *);
extern int ns3_broker_init(void);
extern const struct file_operations *ns3_broker_ops(void);
bool ns3_provisioner_allowed(void);
extern int ns3_socket_new(void *, void *, int, int, u64, void **);
extern void ns3_socket_release(void *);
extern int ns3_bind(void *, const void *, int);
extern int ns3_connect(void *, const void *, int, int);
extern int ns3_listen(void *, int);
extern int ns3_accept(void *, void *, int);
extern int ns3_send(void *, struct msghdr *, size_t);
extern int ns3_recv(void *, struct msghdr *, size_t, int);
extern int ns3_name(void *, void *, int);
extern int ns3_shutdown(void *, int);
extern __poll_t ns3_poll(void *, struct file *, poll_table *);
extern const struct file_operations *ns3_registration_ops(void);
static struct rust_net *rn(struct net *n) { return net_generic(n, net_id); }
static struct rust_sock *rs(struct socket *s) { return container_of(s->sk, struct rust_sock, sk); }

/* Each Rust NativeSock reference owns a sock_hold. Its destructor can safely
 * put the orphaned native sock after application close. No Rust destructor
 * runs from sk_destruct. Rust's application owner is consumed by final release. */
void ns3_hold(void *p);
void ns3_put(void *p);
void ns3_detach_net(struct sock *sk);
int ns3_error(void *p, bool consume);
void ns3_set_error(void *p, int error);
unsigned long ns3_timeout(void *p, bool send, bool nonblock);
void ns3_set_shutdown(void *p, int how);
void *ns3_current_net(void);
void ns3_put_net(void *p);
void *ns3_net_state(void *p);
void *ns3_new_accepted(void *p, int family, u64 generation);
void *ns3_accepted_state(void *p);
void ns3_accept_transfer(void *p, void *new);
void ns3_accept_drop(void *p);
void ns3_sigpipe(void);
struct file *nsrl_anon_file(const struct file_operations *, void *);
void ns3_hold(void *p) { sock_hold(p); }
void ns3_put(void *p) { sock_put(p); }
/* Final application close: provider-held references retain socket memory, not
 * an operational namespace. The caller still owns the native socket reference
 * and has finished all namespace operations before this downgrade. */
void ns3_detach_net(struct sock *sk) {
    struct net *net = sock_net(sk);
    if (!sk->sk_net_refcnt)
        return;
    net_passive_inc(net);
    __netns_tracker_free(net, &sk->ns_tracker, true);
    sk->sk_net_refcnt = 0;
    sock_inuse_add(net, -1);
    __netns_tracker_alloc(net, &sk->ns_tracker, false, GFP_KERNEL);
    put_net(net);
}
int ns3_error(void *p, bool consume) {
	struct sock *sk = p; return consume ? sock_error(sk) : -READ_ONCE(sk->sk_err);
}
void ns3_set_error(void *p, int error) {
	struct sock *sk = p; WRITE_ONCE(sk->sk_err, error);
	sk->sk_error_report(sk);
}
unsigned long ns3_timeout(void *p, bool send, bool nonblock) {
	return send ? sock_sndtimeo(p, nonblock) : sock_rcvtimeo(p, nonblock);
}
void ns3_set_shutdown(void *p, int how) {
    struct sock *sk = p;
    /* Serialize all callers here; Rust's safe shared method needs no caller lock. */
    spin_lock_bh(&sk->sk_lock.slock);
    WRITE_ONCE(sk->sk_shutdown, READ_ONCE(sk->sk_shutdown) | how);
    spin_unlock_bh(&sk->sk_lock.slock);
}
void ns3_sigpipe(void) { send_sig(SIGPIPE, current, 0); }
void *ns3_current_net(void) {
	struct net *n = current->nsproxy->net_ns;
	if (!ns_capable(n->user_ns, CAP_NET_ADMIN)) return ERR_PTR(-EPERM);
	return get_net(n);
}
void ns3_put_net(void *p) { put_net(p); }
void *ns3_net_state(void *p) { return rn(p)->state; }
struct file *nsrl_anon_file(const struct file_operations *ops, void *data) {
	return anon_inode_getfile("netstack3-endpoint", ops, data, O_RDWR | O_NONBLOCK);
}

bool ns3_provisioner_allowed(void) {
    return ns_capable(&init_user_ns, CAP_SYS_ADMIN);
}
extern int ns3_interface_request(void *, const u8 *, u8 *, size_t);
int ns3_interface_ioctl(struct net *, unsigned int, unsigned long);

/* ABI conversion only. No device lookup, flags, addresses or configuration
 * policy live here; namespace-authenticated requests go to the serving worker. */
int ns3_interface_ioctl(struct net *net, unsigned int cmd, unsigned long arg) {
    struct { __le32 command, capable; struct ifreq ifr; } request = {};
    void __user *argp = (void __user *)arg;
    void __user *data;
    int ret;
    BUILD_BUG_ON(sizeof(request) != 48);
    if (cmd != SIOCGIFCONF &&
        !(cmd >= SIOCGIFNAME && cmd <= SIOCGIFCOUNT) &&
        cmd != SIOCGIFTXQLEN && cmd != SIOCSIFTXQLEN &&
        cmd != SIOCGIFMAP && cmd != SIOCSIFMAP)
        return -ENOIOCTLCMD;
    request.command = cpu_to_le32(cmd);
    request.capable = cpu_to_le32(ns_capable_noaudit(net->user_ns, CAP_NET_ADMIN));
    if (cmd == SIOCGIFCONF) {
        struct ifconf conf;
        struct compat_ifconf compat;
        unsigned int width = in_compat_syscall() ? sizeof(struct compat_ifreq) : sizeof(struct ifreq);
        u8 *reply;
        int count, length;
        if (in_compat_syscall()) {
            if (copy_from_user(&compat, argp, sizeof(compat))) return -EFAULT;
            conf.ifc_len = compat.ifc_len;
            conf.ifc_buf = compat_ptr(compat.ifcbuf);
        } else if (copy_from_user(&conf, argp, sizeof(conf))) {
            return -EFAULT;
        }
        if (conf.ifc_len < 0) return -EINVAL;
        reply = kmalloc(4096, GFP_KERNEL);
        if (!reply) return -ENOMEM;
        ret = ns3_interface_request(rn(net)->state, (u8 *)&request, reply, 4096);
        if (ret < 0) goto out;
        if (ret % sizeof(struct ifreq)) { ret = -EPROTO; goto out; }
        count = ret / sizeof(struct ifreq);
        if (conf.ifc_buf) {
            count = min_t(int, count, conf.ifc_len / width);
            for (int i = 0; i < count; i++) {
                if (put_user_ifreq((struct ifreq *)(reply + i * sizeof(struct ifreq)),
                                   conf.ifc_buf + i * width)) {
                    ret = -EFAULT;
                    goto out;
                }
            }
        }
        length = count * width;
        if (in_compat_syscall()) {
            compat.ifc_len = length;
            ret = copy_to_user(argp, &compat, sizeof(compat)) ? -EFAULT : 0;
        } else {
            conf.ifc_len = length;
            ret = copy_to_user(argp, &conf, sizeof(conf)) ? -EFAULT : 0;
        }
out:
        kfree(reply);
        return ret;
    }
    if (get_user_ifreq(&request.ifr, &data, argp)) return -EFAULT;
    ret = ns3_interface_request(rn(net)->state, (u8 *)&request,
                                 (u8 *)&request.ifr, sizeof(request.ifr));
    if (ret < 0) return ret;
    if (ret != sizeof(request.ifr)) return -EPROTO;
    /* Setters consume input; writable input is not part of their ABI. */
    if (cmd == SIOCSIFFLAGS) return 0;
    return put_user_ifreq(&request.ifr, argp);
}

static int release(struct socket *sock) {
	struct rust_sock *s;
	if (!sock->sk) return 0;
	s = rs(sock); ns3_socket_release(s->state); s->state = NULL;
	sock_orphan(&s->sk); sock->sk = NULL;
    ns3_detach_net(&s->sk); sock_put(&s->sk); return 0;
}
static int create(struct net *net, struct socket *sock, int protocol, u64 generation, int family) {
	struct sock *sk; int ret;
	if ((sock->type != SOCK_STREAM || (protocol && protocol != IPPROTO_TCP)) &&
	    (sock->type != SOCK_DGRAM || (protocol && protocol != IPPROTO_UDP))) return -EPROTONOSUPPORT;
	sk = sk_alloc(net, family, GFP_KERNEL, &proto, false);
	if (!sk) return -ENOMEM;
	sock_init_data(sock, sk);
	ret = ns3_socket_new(rn(net)->state, sk, family, sock->type, generation, &rs(sock)->state);
	if (ret) { sock_orphan(sk); sock->sk = NULL; sock_put(sk); return ret; }
	sock->ops = family == AF_INET ? &ops4 : &ops6;
	sock->state = SS_UNCONNECTED;
	sk->sk_protocol = sock->type == SOCK_STREAM ? IPPROTO_TCP : IPPROTO_UDP;
	return 0;
}
void *ns3_new_accepted(void *p, int family, u64 generation) {
    struct net *net = maybe_get_net(sock_net(p));
    struct socket *sock;
    int ret;
    /* The parent may have lost its application reference after PUBLISH's
     * liveness check. A passive sock reference must not resurrect a netns. */
    if (!net) return ERR_PTR(-ENETDOWN);
    ret = sock_create_lite(family, SOCK_STREAM, IPPROTO_TCP, &sock);
    if (!ret) {
        ret = create(net, sock, IPPROTO_TCP, generation, family);
        if (ret) {
            sock_release(sock);
        } else {
            struct sock *parent = p, *child = sock->sk;
            /* These are native wait/buffer settings, not TCP protocol policy.
             * Snapshot under the same lock used by SOL_SOCKET setters. */
            lock_sock(parent);
            child->sk_sndbuf = parent->sk_sndbuf;
            child->sk_rcvbuf = parent->sk_rcvbuf;
            child->sk_userlocks = parent->sk_userlocks;
            child->sk_sndtimeo = parent->sk_sndtimeo;
            child->sk_rcvtimeo = parent->sk_rcvtimeo;
            release_sock(parent);
        }
    }
    put_net(net);
    return ret ? ERR_PTR(ret) : sock;
}

void *ns3_accepted_state(void *p) { return rs(p)->state; }
void ns3_accept_transfer(void *p, void *new) {
	struct socket *old = p, *target = new;
	struct sock *sk = old->sk;
	old->sk = NULL; sock_graft(sk, target); target->state = SS_CONNECTED;
	sock_release(old);
}
void ns3_accept_drop(void *p) { sock_release(p); }
static int create4(struct net *n, struct socket *s, int p, int k) {
	return k ? -EOPNOTSUPP : create(n, s, p, 0, AF_INET);
}
static int create6(struct net *n, struct socket *s, int p, int k) {
	return k ? -EOPNOTSUPP : create(n, s, p, 0, AF_INET6);
}
static int bind(struct socket *s, struct sockaddr_unsized *a, int len) { return ns3_bind(rs(s)->state, a, len); }
static int connect(struct socket *s, struct sockaddr_unsized *a, int len, int flags) {
	int ret = ns3_connect(rs(s)->state, a, len, flags);
	if (!ret) s->state = SS_CONNECTED;
	return ret;
}
static int listen(struct socket *s, int n) { return ns3_listen(rs(s)->state, n); }
static int accept(struct socket *s, struct socket *new, struct proto_accept_arg *arg) {
	return ns3_accept(rs(s)->state, new, arg->flags);
}
static int sendmsg(struct socket *s, struct msghdr *m, size_t n) { return ns3_send(rs(s)->state, m, n); }
static int recvmsg(struct socket *s, struct msghdr *m, size_t n, int flags) { return ns3_recv(rs(s)->state, m, n, flags); }
static int socket_getname(struct socket *s, struct sockaddr *a, int peer) { return ns3_name(rs(s)->state, a, peer); }
static int shutdown(struct socket *s, int how) { return ns3_shutdown(rs(s)->state, how); }
static __poll_t poll(struct file *f, struct socket *s, poll_table *t) { return ns3_poll(rs(s)->state, f, t); }
static int setsockopt(struct socket *s, int l, int o, sockptr_t p, unsigned int n) { return -ENOPROTOOPT; }
#define OPS(f) { .family = f, .owner = THIS_MODULE, .release = release, .bind = bind, \
	.connect = connect, .listen = listen, .accept = accept, .sendmsg = sendmsg, .recvmsg = recvmsg, \
	.getname = socket_getname, .shutdown = shutdown, .poll = poll, .setsockopt = setsockopt, \
	.socketpair = sock_no_socketpair, .ioctl = sock_no_ioctl, .mmap = sock_no_mmap }
static const struct proto_ops ops4 = OPS(AF_INET), ops6 = OPS(AF_INET6);
static const struct net_proto_family family4 = { .family = AF_INET, .create = create4, .owner = THIS_MODULE };
static const struct net_proto_family family6 = { .family = AF_INET6, .create = create6, .owner = THIS_MODULE };
static int __net_init net_init(struct net *net) {
	rn(net)->state = ns3_net_new(); return rn(net)->state ? 0 : -ENOMEM;
}
static void __net_exit net_exit(struct net *net) { ns3_net_drop(rn(net)->state); }
static struct pernet_operations pernet = { .init = net_init, .exit = net_exit, .id = &net_id, .size = sizeof(struct rust_net) };
extern int ns3_nl_socket_new(void *, void *, void **);
int ns3_nl_open(struct sock *sk, void **out);
int ns3_nl_open(struct sock *sk, void **out) {
    return ns3_nl_socket_new(rn(sock_net(sk))->state, sk, out);
}
static struct miscdevice broker_device = { .minor = MISC_DYNAMIC_MINOR,
    .name = "netstack3-namespaces", .mode = 0600 };
static struct miscdevice device = { .minor = MISC_DYNAMIC_MINOR, .name = "netstack3", .mode = 0600 };
static int __init init(void) {
	int ret = ns3_broker_init();
    if (ret) return ret;
    ret = proto_register(&proto, 1);
	if (ret) return ret;
	ret = register_pernet_subsys(&pernet); if (ret) goto proto;
	ret = sock_register(&family4); if (ret) goto pernet;
	ret = sock_register(&family6); if (ret) goto family4;
	device.fops = ns3_registration_ops();
	ret = misc_register(&device);
    if (!ret) {
        broker_device.fops = ns3_broker_ops();
        ret = misc_register(&broker_device);
        if (!ret) return 0;
        misc_deregister(&device);
    }
	sock_unregister(AF_INET6);
family4: sock_unregister(AF_INET);
pernet: unregister_pernet_subsys(&pernet);
proto: proto_unregister(&proto); return ret;
}
subsys_initcall(init);
MODULE_LICENSE("GPL");
