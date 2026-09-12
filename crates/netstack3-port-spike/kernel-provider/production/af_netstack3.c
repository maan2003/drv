// SPDX-License-Identifier: GPL-2.0-only
/*
 * Userspace-backed Internet socket frontend. No IP or TCP implementation.
 * Socket buffers, file lifetime, waits and namespace authority stay in Linux.
 * Protocol behavior stays in the provider. See ARCH-network-service.
 */
#include <linux/module.h>
#include <linux/net.h>
#include <linux/in.h>
#include <linux/in6.h>
#include <linux/inet.h>
#include <linux/miscdevice.h>
#include <linux/poll.h>
#include <linux/slab.h>
#include <linux/uio.h>
#include <linux/anon_inodes.h>
#include <linux/file.h>
#include <linux/unaligned.h>
#include <linux/completion.h>
#include <linux/nsproxy.h>
#include <linux/sched/signal.h>
#include <net/sock.h>
#include <net/net_namespace.h>
#include <net/netns/generic.h>
#include "protocol.h"

#define NS3_REQUESTS 32
#define NS3_SOCKETS 256
#define NS3_BUFFER (256 * 1024)
#define NS3_CONTROL_TIMEOUT (10 * HZ)

struct ns3_net {
	struct mutex lock;
	wait_queue_head_t wait;
	struct list_head sockets;
	u64 next_socket, generation;
	atomic_t socket_count;
	bool online;
};
struct ns3_sock {
	struct sock sk;
	struct list_head node, requests, accepted, accept_node;
	struct socket *accept_socket;
	unsigned int accept_count, backlog;
	bool accept_space;
	struct mutex lock;
	wait_queue_head_t provider_wait;
	u64 id, generation, next_request;
	bool dead, opened, connected, listening, eof, connecting;
	bool claimed, app_closed, close_read;
	unsigned int pending, credit_return, rx_credit;
	unsigned int tx_bytes, rx_offset;
	struct ns3_addr local, peer;
	struct mutex control, receive, transmit;
};
struct ns3_request {
	struct list_head node;
	struct completion done;
	struct ns3_sock *owner;
	struct ns3_msg header;
	void *data, *reply;
	size_t reply_len, credit;
	bool read, async, completed;
	int error;
};
struct ns3_session { struct net *net; u64 generation; };
static unsigned int ns3_net_id;
static struct proto ns3_proto;
static const struct proto_ops ns3_ops4, ns3_ops6;

static struct ns3_sock *ns3_sk(struct socket *s)
{
	return container_of(s->sk, struct ns3_sock, sk);
}
static struct ns3_net *ns3_net(struct sock *sk)
{
	return net_generic(sock_net(sk), ns3_net_id);
}
static bool ns3_alive(struct ns3_net *n, struct ns3_sock *s)
{
	return READ_ONCE(n->online) && !READ_ONCE(s->dead) && s->generation == READ_ONCE(n->generation);
}
static void ns3_free_request(struct ns3_request *r)
{
	sock_put(&r->owner->sk);
	kfree(r->data);
	kfree(r->reply);
	kfree(r);
}
/* Socket-local failure never revokes other endpoints. s->lock held. */
static void ns3_abort_socket(struct ns3_sock *s)
{
	struct ns3_request *r, *tmp;
	s->dead = true;
	WRITE_ONCE(s->sk.sk_err, ENETDOWN);
	s->sk.sk_error_report(&s->sk);
	s->sk.sk_state_change(&s->sk);
	list_for_each_entry_safe(r, tmp, &s->requests, node) {
		list_del_init(&r->node);
		s->pending--;
		s->tx_bytes -= r->credit;
		r->error = -ENETDOWN;
		r->completed = true;
		if (r->async) ns3_free_request(r);
		else complete(&r->done);
	}
	wake_up_interruptible_all(&s->provider_wait);
}
/* Registration lock held; endpoint operations never acquire it. */
static void ns3_abort(struct ns3_net *n)
{
	struct ns3_sock *s;
	n->online = false;
	list_for_each_entry(s, &n->sockets, node) {
		mutex_lock(&s->lock);
		ns3_abort_socket(s);
		mutex_unlock(&s->lock);
	}
	wake_up_interruptible_all(&n->wait);
}
/* Request allocation and admission are bounded even after userspace reads.
 * All outstanding requests retain their socket independently of its file.
 */
static struct ns3_request *ns3_submit(struct ns3_sock *s, u32 op,
				     const void *data, size_t len, bool async,
				     size_t credit)
{
	struct ns3_net *n = ns3_net(&s->sk);
	struct ns3_request *r;
	if (len > NS3_PAYLOAD + sizeof(struct ns3_addr))
		return ERR_PTR(-EMSGSIZE);
	r = kzalloc(sizeof(*r), GFP_KERNEL);
	if (!r)
		return ERR_PTR(-ENOMEM);
	r->data = kmemdup(data, len, GFP_KERNEL);
	if (len && !r->data) { kfree(r); return ERR_PTR(-ENOMEM); }
	init_completion(&r->done);
	INIT_LIST_HEAD(&r->node);
	mutex_lock(&s->lock);
	if (!ns3_alive(n, s) || s->pending >= (credit ? NS3_REQUESTS - 8 : NS3_REQUESTS)) {
		int err = ns3_alive(n, s) ? -EAGAIN : -ENETDOWN;
		mutex_unlock(&s->lock);
		kfree(r->data); kfree(r); return ERR_PTR(err);
	}
	if (credit && s->tx_bytes + credit > NS3_BUFFER) {
		mutex_unlock(&s->lock);
		kfree(r->data); kfree(r); return ERR_PTR(-EAGAIN);
	}
	sock_hold(&s->sk);
	r->owner = s; r->async = async; r->credit = credit;
	r->header = (struct ns3_msg) {
		.version = cpu_to_le32(NS3_VERSION), .op = cpu_to_le32(op),
		.socket = cpu_to_le64(s->id),
		.request = cpu_to_le64(++s->next_request),
		.len = cpu_to_le32(len),
	};
	s->tx_bytes += credit;
	list_add_tail(&r->node, &s->requests);
	s->pending++;
	mutex_unlock(&s->lock);
	wake_up_interruptible(&s->provider_wait);
	return r;
}
static int ns3_call(struct ns3_sock *s, u32 op, const void *data, size_t len,
		    void *reply, size_t capacity)
{
	struct ns3_request *r = ns3_submit(s, op, data, len, false, 0);
	long waited;
	int ret;
	if (IS_ERR(r)) return PTR_ERR(r);
	waited = wait_for_completion_interruptible_timeout(&r->done, NS3_CONTROL_TIMEOUT);
	mutex_lock(&s->lock);
	/* Interrupted control may already have taken effect remotely. Revoke the
	 * socket instead of freeing an ID and accepting an ambiguous late reply.
	 */
	if (!r->completed) ns3_abort_socket(s);
	ret = waited < 0 ? (int)waited : !waited ? -ETIMEDOUT : r->error;
	if (!ret && r->reply_len > capacity) ret = -EPROTO;
	if (!ret && r->reply_len) memcpy(reply, r->reply, r->reply_len);
	if (!ret) ret = r->reply_len;
	mutex_unlock(&s->lock);
	ns3_free_request(r);
	return ret;
}
static int ns3_open_remote(struct ns3_sock *s)
{
	__le32 type[2] = { cpu_to_le32(s->sk.sk_type), cpu_to_le32(s->sk.sk_family == AF_INET ? 4 : 6) };
	int ret;
	if (s->opened) return 0;
	struct ns3_request *r = ns3_submit(s, NS3_OPEN, &type, sizeof(type), true, 0);
	ret = IS_ERR(r) ? PTR_ERR(r) : 0;
	if (!ret) s->opened = true;
	return ret;
}
static int ns3_address(struct sockaddr *a, int len, struct ns3_addr *out)
{
	memset(out, 0, sizeof(*out));
	if (a->sa_family == AF_INET && len >= sizeof(struct sockaddr_in)) {
		struct sockaddr_in *v = (void *)a;
		out->family = cpu_to_le16(4);
		out->port = cpu_to_le16(ntohs(v->sin_port));
		memcpy(out->address, &v->sin_addr, 4);
		return 0;
	}
	if (a->sa_family == AF_INET6 && len >= sizeof(struct sockaddr_in6)) {
		struct sockaddr_in6 *v = (void *)a;
		out->family = cpu_to_le16(6);
		out->port = cpu_to_le16(ntohs(v->sin6_port));
		memcpy(out->address, &v->sin6_addr, 16);
		out->scope = cpu_to_le32(v->sin6_scope_id);
		return v->sin6_scope_id ? -EOPNOTSUPP : 0;
	}
	return -EINVAL;
}
static int ns3_bind(struct socket *sock, struct sockaddr *addr, int len)
{
	struct ns3_sock *s = ns3_sk(sock);
	struct ns3_addr a;
	int ret = ns3_address(addr, len, &a);
	if (ret) return ret;
	if (addr->sa_family != sock->ops->family) return -EAFNOSUPPORT;
	mutex_lock(&s->control);
	ret = ns3_open_remote(s);
	if (!ret) ret = ns3_call(s, NS3_BIND, &a, sizeof(a), &s->local, sizeof(a));
	mutex_unlock(&s->control);
	return ret < 0 ? ret : ret == sizeof(a) ? 0 : -EPROTO;
}
static int ns3_listen(struct socket *sock, int backlog)
{
	struct ns3_sock *s = ns3_sk(sock);
	__le32 value = cpu_to_le32(min(backlog, NS3_SOCKETS));
	int ret;
	if (sock->type != SOCK_STREAM) return -EOPNOTSUPP;
	mutex_lock(&s->control);
	ret = ns3_open_remote(s);
	if (!ret) ret = ns3_call(s, NS3_LISTEN, &value, sizeof(value), &s->local, sizeof(s->local));
	if (ret == sizeof(s->local)) {
		mutex_lock(&s->lock); s->listening = true; s->backlog = max(backlog, 1); s->accept_space = true;
		mutex_unlock(&s->lock); wake_up_interruptible(&s->provider_wait); ret = 0;
	}
	else if (ret >= 0) ret = -EPROTO;
	mutex_unlock(&s->control);
	return ret;
}
static int ns3_connect(struct socket *sock, struct sockaddr *addr, int len, int flags)
{
	struct ns3_sock *s = ns3_sk(sock);
	struct ns3_addr a;
	struct ns3_request *r;
	long timeo;
	long ret = ns3_address(addr, len, &a);
	if (ret) return ret;
	if (addr->sa_family != sock->ops->family) return -EAFNOSUPPORT;
	mutex_lock(&s->control);
	if (s->connected) { ret = -EISCONN; goto out; }
	if (s->connecting) { ret = -EALREADY; goto out; }
	ret = ns3_open_remote(s);
	if (ret) goto out;
	s->peer = a;
	s->connecting = true;
	r = ns3_submit(s, NS3_CONNECT, &a, sizeof(a), true, 0);
	if (IS_ERR(r)) { s->connecting = false; ret = PTR_ERR(r); goto out; }
	/* UDP connect configures a peer; it has no transport handshake to defer. */
	if (sock->type == SOCK_STREAM && flags & O_NONBLOCK) { ret = -EINPROGRESS; goto out; }
	timeo = sock_sndtimeo(&s->sk, false);
	ret = wait_event_interruptible_timeout(*sk_sleep(&s->sk),
			!READ_ONCE(s->connecting) || READ_ONCE(s->dead), timeo);
	if (ret > 0) ret = s->dead ? -ENETDOWN : s->connected ? 0 : sock_error(&s->sk);
	else if (!ret) ret = -EINPROGRESS;
out:
	mutex_unlock(&s->control);
	return ret;
}
static int ns3_create(struct net *net, struct socket *sock, int protocol, int kern, int family);
static int ns3_accept(struct socket *sock, struct socket *new, struct proto_accept_arg *arg)
{
	struct ns3_sock *s = ns3_sk(sock), *child;
	struct socket *temporary;
	long ret, timeo = sock_rcvtimeo(&s->sk, arg->flags & O_NONBLOCK);
	mutex_lock(&s->lock);
	for (;;) {
		if (!s->listening) { ret = -EINVAL; break; }
		if (s->dead) { ret = -ENETDOWN; break; }
		if (!list_empty(&s->accepted)) {
			child = list_first_entry(&s->accepted, struct ns3_sock, accept_node);
			list_del_init(&child->accept_node); s->accept_count--;
			temporary = child->accept_socket; child->accept_socket = NULL;
			temporary->sk = NULL;
			sock_graft(&child->sk, new); new->state = SS_CONNECTED;
			s->accept_space = true;
			wake_up_interruptible(&s->provider_wait);
			mutex_unlock(&s->lock);
			sock_release(temporary);
			return 0;
		}
		if (!timeo) { ret = -EAGAIN; break; }
		mutex_unlock(&s->lock);
		ret = wait_event_interruptible_timeout(*sk_sleep(&s->sk),
			READ_ONCE(s->accept_count) || READ_ONCE(s->dead), timeo);
		mutex_lock(&s->lock);
		if (ret <= 0) { if (!ret) ret = -EAGAIN; break; }
		timeo = ret;
	}
	mutex_unlock(&s->lock);
	return ret;
}
static int ns3_sendmsg(struct socket *sock, struct msghdr *msg, size_t len)
{
	struct ns3_sock *s = ns3_sk(sock);
	struct ns3_addr dest = {};
	struct ns3_request *r;
	u8 *data;
	size_t count, done = 0;
	long timeo = sock_sndtimeo(&s->sk, msg->msg_flags & MSG_DONTWAIT);
	long ret = 0;
	if (msg->msg_flags & ~(MSG_DONTWAIT | MSG_NOSIGNAL | MSG_MORE))
		return -EOPNOTSUPP;
	if (sock->type == SOCK_DGRAM && len > NS3_PAYLOAD) return -EMSGSIZE;
	mutex_lock(&s->transmit);
	if (s->sk.sk_shutdown & SEND_SHUTDOWN) { ret = -EPIPE; goto out; }
	if (msg->msg_name) {
		ret = ns3_address(msg->msg_name, msg->msg_namelen, &dest);
		if (ret) goto out;
	}
	mutex_lock(&s->control);
	ret = ns3_open_remote(s);
	mutex_unlock(&s->control);
	if (ret) goto out;
	if (sock->type == SOCK_STREAM && !s->connected) { ret = -ENOTCONN; goto out; }
	do {
		count = min_t(size_t, len - done, NS3_PAYLOAD);
		if (s->dead) { ret = -ENETDOWN; break; }
		data = kmalloc(sizeof(dest) + count, GFP_KERNEL);
		if (!data) { ret = -ENOMEM; break; }
		memcpy(data, &dest, sizeof(dest));
		/* Revert iterator on failed admission; never lose unaccepted bytes. */
		if (!copy_from_iter_full(data + sizeof(dest), count, &msg->msg_iter)) {
			kfree(data); ret = -EFAULT; break;
		}
		r = ns3_submit(s, NS3_SEND, data, sizeof(dest) + count, true, count ?: 1);
		kfree(data);
		if (!IS_ERR(r)) { done += count; continue; }
		iov_iter_revert(&msg->msg_iter, count);
		ret = PTR_ERR(r);
		if (ret != -EAGAIN || !timeo || done) break;
		ret = wait_event_interruptible_timeout(*sk_sleep(&s->sk),
			(READ_ONCE(s->tx_bytes) + (count ?: 1) <= NS3_BUFFER && READ_ONCE(s->pending) < NS3_REQUESTS - 8) ||
			READ_ONCE(s->dead), timeo);
		if (ret <= 0) { if (!ret) ret = -EAGAIN; break; }
		timeo = ret;
	} while (done < len);
out:
	mutex_unlock(&s->transmit);
	if (ret == -EPIPE && !(msg->msg_flags & MSG_NOSIGNAL)) send_sig(SIGPIPE, current, 0);
	return done ? done : ret;
}
static int ns3_recvmsg(struct socket *sock, struct msghdr *msg, size_t len, int flags)
{
	struct ns3_sock *s = ns3_sk(sock);
	struct sk_buff *skb;
	struct ns3_addr *addr;
	size_t done = 0, count, available;
	long ret = 0;

	long timeo = sock_rcvtimeo(&s->sk, flags & MSG_DONTWAIT);
	if (flags & ~(MSG_DONTWAIT | MSG_PEEK | MSG_WAITALL | MSG_TRUNC)) return -EOPNOTSUPP;
	if (!len && sock->type == SOCK_STREAM) return 0;
	mutex_lock(&s->receive);
	for (;;) {
		mutex_lock(&s->lock);
		skb = skb_peek(&s->sk.sk_receive_queue);
		if (skb) {
			addr = (void *)skb->data;
			available = skb->len - sizeof(*addr) - s->rx_offset;
			count = min(len - done, available);
			if (!copy_to_iter_full(skb->data + sizeof(*addr) + s->rx_offset, count, &msg->msg_iter))
				ret = -EFAULT;
			else {
				done += count;
				if (sock->type == SOCK_DGRAM) {
					if (msg->msg_name) {
						struct sockaddr_in *a4 = msg->msg_name;
						struct sockaddr_in6 *a6 = msg->msg_name;
						if (le16_to_cpu(addr->family) == 4) {
							memset(a4, 0, sizeof(*a4)); a4->sin_family = AF_INET;
							a4->sin_port = htons(le16_to_cpu(addr->port));
							memcpy(&a4->sin_addr, addr->address, 4); msg->msg_namelen = sizeof(*a4);
						} else {
							memset(a6, 0, sizeof(*a6)); a6->sin6_family = AF_INET6;
							a6->sin6_port = htons(le16_to_cpu(addr->port));
							memcpy(&a6->sin6_addr, addr->address, 16); msg->msg_namelen = sizeof(*a6);
						}
					}
					if (count < available) msg->msg_flags |= MSG_TRUNC;
					if (flags & MSG_TRUNC) done = available;
				}
				if (!(flags & MSG_PEEK)) {
					s->rx_offset += count;
					if (count == available || sock->type == SOCK_DGRAM) {
						skb_unlink(skb, &s->sk.sk_receive_queue); kfree_skb(skb); s->rx_offset = 0; s->credit_return++;
					}
					wake_up_interruptible(&s->provider_wait);
				}
			}
			mutex_unlock(&s->lock);

			if (ret || sock->type == SOCK_DGRAM || flags & MSG_PEEK || done == len || !(flags & MSG_WAITALL)) break;
			continue;
		}
		if (s->dead) ret = -ENETDOWN;
		else if (s->sk.sk_err) ret = sock_error(&s->sk);
		else if (s->eof || s->sk.sk_shutdown & RCV_SHUTDOWN) ret = 0;
		else ret = -EAGAIN;
		mutex_unlock(&s->lock);
		if (ret != -EAGAIN || !timeo || (done && !(flags & MSG_WAITALL))) break;
		ret = wait_event_interruptible_timeout(*sk_sleep(&s->sk),
			!skb_queue_empty(&s->sk.sk_receive_queue) || READ_ONCE(s->eof) ||
			READ_ONCE(s->dead) || READ_ONCE(s->sk.sk_err) ||
			(READ_ONCE(s->sk.sk_shutdown) & RCV_SHUTDOWN), timeo);
		if (ret <= 0) { if (!ret) ret = -EAGAIN; break; }
		timeo = ret;
	}
	mutex_unlock(&s->receive);
	return done ? done : ret;
}
static __poll_t ns3_poll(struct file *file, struct socket *sock, poll_table *wait)
{
	struct ns3_sock *s = ns3_sk(sock);
	__poll_t mask = 0;
	poll_wait(file, sk_sleep(&s->sk), wait);
	mutex_lock(&s->lock);
	if (s->dead || s->sk.sk_err) mask |= EPOLLERR;
	if (s->dead) mask |= EPOLLHUP;
	if (!skb_queue_empty(&s->sk.sk_receive_queue) || s->eof || s->accept_count || s->sk.sk_shutdown & RCV_SHUTDOWN)
		mask |= EPOLLIN | EPOLLRDNORM;
	if (s->eof && !s->listening) mask |= EPOLLRDHUP;
	if (!s->dead && !s->connecting && !s->listening &&
	    (s->connected || sock->type == SOCK_DGRAM) &&
	    s->tx_bytes < NS3_BUFFER && s->pending < NS3_REQUESTS - 8)
		mask |= EPOLLOUT | EPOLLWRNORM;
	mutex_unlock(&s->lock);
	return mask;
}
static int ns3_getname(struct socket *sock, struct sockaddr *addr, int peer)
{
	struct ns3_sock *s = ns3_sk(sock);
	struct ns3_addr a;
	__le32 which = cpu_to_le32(peer);
	int ret;
	mutex_lock(&s->control);
	ret = s->opened ? ns3_call(s, NS3_GETNAME, &which, sizeof(which), &a, sizeof(a)) : -ENOTCONN;
	mutex_unlock(&s->control);
	if (ret < 0) return ret;
	if (ret != sizeof(a)) return -EPROTO;
	if (sock->ops->family == AF_INET) {
		struct sockaddr_in *v = (void *)addr;
		memset(v, 0, sizeof(*v)); v->sin_family = AF_INET;
		v->sin_port = htons(le16_to_cpu(a.port)); memcpy(&v->sin_addr, a.address, 4);
		return sizeof(*v);
	} else {
		struct sockaddr_in6 *v = (void *)addr;
		memset(v, 0, sizeof(*v)); v->sin6_family = AF_INET6;
		v->sin6_port = htons(le16_to_cpu(a.port)); memcpy(&v->sin6_addr, a.address, 16);
		return sizeof(*v);
	}
}
static int ns3_shutdown(struct socket *sock, int how)
{
	struct ns3_sock *s = ns3_sk(sock);
	__le32 value = cpu_to_le32(how + 1);
	int ret;
	if (how < SHUT_RD || how > SHUT_RDWR) return -EINVAL;
	mutex_lock(&s->transmit);
	mutex_lock(&s->control);
	ret = wait_event_interruptible(*sk_sleep(&s->sk), !READ_ONCE(s->tx_bytes) || READ_ONCE(s->dead));
	if (ret) goto out;
	ret = ns3_call(s, NS3_SHUTDOWN, &value, sizeof(value), NULL, 0);
	if (!ret) { s->sk.sk_shutdown |= how + 1; s->sk.sk_state_change(&s->sk); }
out:
	mutex_unlock(&s->control);
	mutex_unlock(&s->transmit);
	return ret;
}
static int ns3_release(struct socket *sock)
{
	struct ns3_sock *s;
	struct ns3_net *n;
	struct ns3_sock *child, *tmp;
	LIST_HEAD(accepted);
	if (!sock->sk) return 0;
	s = ns3_sk(sock); n = ns3_net(&s->sk);
	mutex_lock(&n->lock);
	list_del_init(&s->node);
	mutex_lock(&s->lock);
	s->app_closed = true;
	list_splice_init(&s->accepted, &accepted); s->accept_count = 0;
	skb_queue_purge(&s->sk.sk_receive_queue);
	if (!s->claimed) ns3_abort_socket(s);
	wake_up_interruptible_all(&s->provider_wait);
	mutex_unlock(&s->lock);
	mutex_unlock(&n->lock);
	list_for_each_entry_safe(child, tmp, &accepted, accept_node) {
		list_del_init(&child->accept_node); sock_release(child->accept_socket);
	}
	sock_orphan(&s->sk); sock->sk = NULL; sock_put(&s->sk);
	return 0;
}
static int ns3_setsockopt(struct socket *sock, int level, int opt,
			  sockptr_t val, unsigned int len)
{
	/* Unsupported protocol options use the socket ABI errno; do not claim
	 * support for options (such as IP_RECVERR) that are not implemented. */
	return -ENOPROTOOPT;
}
#define NS3_OPS(fam) { .family = fam, .owner = THIS_MODULE, \
	.release = ns3_release, .bind = ns3_bind, .connect = ns3_connect, \
	.socketpair = sock_no_socketpair, .accept = ns3_accept, .listen = ns3_listen, \
	.getname = ns3_getname, .poll = ns3_poll, .ioctl = sock_no_ioctl, \
	.shutdown = ns3_shutdown, .setsockopt = ns3_setsockopt, \
	.sendmsg = ns3_sendmsg, .recvmsg = ns3_recvmsg, .mmap = sock_no_mmap }
static const struct proto_ops ns3_ops4 = NS3_OPS(PF_INET);
static const struct proto_ops ns3_ops6 = NS3_OPS(PF_INET6);
static struct proto ns3_proto = {
	.name = "NETSTACK3", .owner = THIS_MODULE, .obj_size = sizeof(struct ns3_sock),
};
static void ns3_destruct(struct sock *sk)
{
	/* Keep the quota charged while provider FDs or queued requests retain the
	 * socket, not merely until the application closes its file. */
	atomic_dec(&ns3_net(sk)->socket_count);
}
static int ns3_create(struct net *net, struct socket *sock, int protocol, int kern, int family)
{
	struct ns3_net *n = net_generic(net, ns3_net_id);
	struct sock *sk;
	struct ns3_sock *s;
	/* Internal accepted endpoints use kern=1, external kernel clients remain unsupported by family wrappers. */
	if ((sock->type != SOCK_STREAM || (protocol && protocol != IPPROTO_TCP)) &&
	    (sock->type != SOCK_DGRAM || (protocol && protocol != IPPROTO_UDP)))
		return -EPROTONOSUPPORT;
	sk = sk_alloc(net, family, GFP_KERNEL, &ns3_proto, false);
	if (!sk) return -ENOMEM;
	sock_init_data(sock, sk);
	s = container_of(sk, struct ns3_sock, sk);
	mutex_init(&s->lock); init_waitqueue_head(&s->provider_wait);
	INIT_LIST_HEAD(&s->requests); INIT_LIST_HEAD(&s->accepted); INIT_LIST_HEAD(&s->accept_node); s->rx_credit = 4;
	mutex_init(&s->control);
	mutex_init(&s->receive); mutex_init(&s->transmit);
	INIT_LIST_HEAD(&s->node);
	sock->ops = family == PF_INET ? &ns3_ops4 : &ns3_ops6;
	sock->state = SS_UNCONNECTED;
	sk->sk_protocol = sock->type == SOCK_STREAM ? IPPROTO_TCP : IPPROTO_UDP;
	mutex_lock(&n->lock);
	if (!n->online || atomic_read(&n->socket_count) >= NS3_SOCKETS) {
		int err = n->online ? -ENFILE : -ENETDOWN;
		mutex_unlock(&n->lock); sock_orphan(sk); sock->sk = NULL; sock_put(sk); return err;
	}
	s->claimed = kern;
	s->id = ++n->next_socket; s->generation = n->generation;
	s->local.family = cpu_to_le16(family == AF_INET ? 4 : 6);
	list_add_tail(&s->node, &n->sockets); atomic_inc(&n->socket_count);
	sk->sk_destruct = ns3_destruct;
	mutex_unlock(&n->lock);
	wake_up_interruptible(&n->wait);
	return 0;
}
static int ns3_create4(struct net *n, struct socket *s, int p, int k) { return k ? -EOPNOTSUPP : ns3_create(n,s,p,0,PF_INET); }
static int ns3_create6(struct net *n, struct socket *s, int p, int k) { return k ? -EOPNOTSUPP : ns3_create(n,s,p,0,PF_INET6); }
static const struct net_proto_family ns3_family4 = { .family = PF_INET, .create = ns3_create4, .owner = THIS_MODULE };
static const struct net_proto_family ns3_family6 = { .family = PF_INET6, .create = ns3_create6, .owner = THIS_MODULE };

static bool ns3_unread(struct ns3_sock *s)
{
	struct ns3_request *r;
	list_for_each_entry(r, &s->requests, node) if (!r->read) return true;
	return false;
}
static ssize_t ns3_read(struct file *file, char __user *buf, size_t len, loff_t *off)
{
	struct ns3_sock *s = file->private_data;
	struct ns3_request *r;
	int ret = -EAGAIN;
	mutex_lock(&s->lock);
	if (s->dead) { ret = -ENETDOWN; goto out; }
	list_for_each_entry(r, &s->requests, node) {
		if (r->read) continue;
		if (len < sizeof(r->header) + le32_to_cpu(r->header.len)) { ret = -EMSGSIZE; break; }
		if (copy_to_user(buf, &r->header, sizeof(r->header)) ||
		    copy_to_user(buf + sizeof(r->header), r->data, le32_to_cpu(r->header.len))) { ret = -EFAULT; break; }
		r->read = true;
		ret = sizeof(r->header) + le32_to_cpu(r->header.len); break;
	}
	if (ret == -EAGAIN && (s->accept_space || s->credit_return || (s->app_closed && !s->close_read))) {
		struct ns3_msg h = { .version = cpu_to_le32(NS3_VERSION), .socket = cpu_to_le64(s->id) };
		__le32 credits = cpu_to_le32(s->credit_return);
		bool accepting = s->accept_space;
		bool closing = !accepting && !s->credit_return;
		h.op = cpu_to_le32(accepting ? NS3_ACCEPT : closing ? NS3_CLOSE : NS3_CREDIT);
		h.len = cpu_to_le32(closing ? 0 : sizeof(credits));
		ret = sizeof(h) + (closing ? 0 : sizeof(credits));
		if (len < ret) ret = -EMSGSIZE;
		else if (copy_to_user(buf, &h, sizeof(h)) ||
			 (!closing && copy_to_user(buf + sizeof(h), &credits, sizeof(credits)))) ret = -EFAULT;
		else if (accepting) s->accept_space = false;
		else if (closing) s->close_read = true;
		else { s->rx_credit += s->credit_return; s->credit_return = 0; }
	}
out:
	mutex_unlock(&s->lock);
	return ret;
}
static ssize_t ns3_write(struct file *file, const char __user *buf, size_t len, loff_t *off)
{
	struct ns3_sock *s = file->private_data;
	struct ns3_msg *h;
	struct ns3_request *r, *tmp;
	struct sk_buff *skb;
	void *data;
	size_t size;
	u32 op, status;
	int ret = -EPROTO;
	if (len < sizeof(*h) || len > sizeof(*h) + NS3_PAYLOAD + sizeof(struct ns3_addr)) return -EMSGSIZE;
	h = memdup_user(buf, len);
	if (IS_ERR(h)) return PTR_ERR(h);
	size = le32_to_cpu(h->len); op = le32_to_cpu(h->op); status = le32_to_cpu(h->status);
	if (le32_to_cpu(h->version) != NS3_VERSION || size != len - sizeof(*h) || status > 4095) goto free;
	data = h + 1;
	if (h->socket != cpu_to_le64(s->id)) goto free;
	mutex_lock(&s->lock);
	if (s->dead) { ret = -ENETDOWN; goto out; }
	if (!h->request) {
		{
			if (op == NS3_RX && size >= sizeof(struct ns3_addr)) {
				if (!s->rx_credit) { ret = -EAGAIN; goto out; }
				if (atomic_read(&s->sk.sk_rmem_alloc) + size + SKB_DATA_ALIGN(sizeof(struct sk_buff)) > NS3_BUFFER) { ret = -EAGAIN; goto out; }
				skb = alloc_skb(size, GFP_KERNEL);
				if (!skb) { ret = -ENOMEM; goto out; }
				skb_put_data(skb, data, size);
				skb_set_owner_r(skb, &s->sk);
				skb_queue_tail(&s->sk.sk_receive_queue, skb); s->rx_credit--;
				s->sk.sk_data_ready(&s->sk);
			} else if (op == NS3_STATE && size == 4) {
				u32 state = get_unaligned_le32(data);
				if (state & NS3_CONNECTED) { s->connected = true; s->connecting = false; }
				if (state & NS3_EOF) s->eof = true;
				if (status) { s->connecting = false; WRITE_ONCE(s->sk.sk_err, status); s->sk.sk_error_report(&s->sk); }
				s->sk.sk_state_change(&s->sk);
			} else goto bad;
			ret = len; goto out;
		}
	}
	list_for_each_entry_safe(r, tmp, &s->requests, node) {
		if (r->header.request != h->request) continue;
		if (!r->read || r->header.socket != h->socket || r->header.op != h->op) goto bad;
		r->reply = kmemdup(data, size, GFP_KERNEL);
		if (size && !r->reply) { ret = -ENOMEM; goto out; }
		r->reply_len = size; r->error = -(int)status;
		list_del_init(&r->node); s->pending--;
		r->owner->tx_bytes -= r->credit;
		if (r->async && status && !(op == NS3_CONNECT && status == EINPROGRESS)) {
			WRITE_ONCE(r->owner->sk.sk_err, status);
			r->owner->connecting = false;
			r->owner->sk.sk_error_report(&r->owner->sk);
		}
		if (op == NS3_CONNECT && !status && r->owner->sk.sk_type == SOCK_DGRAM)
			r->owner->connected = true, r->owner->connecting = false;
		r->owner->sk.sk_write_space(&r->owner->sk);
		r->completed = true;
		if (r->async) ns3_free_request(r); else complete(&r->done);

		wake_up_interruptible(&s->provider_wait);
		ret = len; goto out;
	}
bad:
	ns3_abort_socket(s);
out:
	mutex_unlock(&s->lock);
free:
	kfree(h); return ret;
}
static __poll_t ns3_endpoint_poll(struct file *f, poll_table *wait)
{
	struct ns3_sock *s = f->private_data;
	__poll_t mask;
	poll_wait(f, &s->provider_wait, wait);
	mutex_lock(&s->lock);
	mask = s->dead ? EPOLLERR | EPOLLHUP : EPOLLOUT |
		((ns3_unread(s) || s->accept_space || s->credit_return || (s->app_closed && !s->close_read)) ? EPOLLIN : 0);
	mutex_unlock(&s->lock);
	return mask;
}
static int ns3_endpoint_release(struct inode *inode, struct file *f)
{
	struct ns3_sock *s = f->private_data;
	mutex_lock(&s->lock);
	ns3_abort_socket(s);
	mutex_unlock(&s->lock);
	sock_put(&s->sk);
	return 0;
}
static const struct file_operations ns3_endpoint_fops;
static long ns3_publish_accept(struct file *f, unsigned int cmd, unsigned long arg)
{
	struct ns3_sock *listener = f->private_data, *child;
	struct ns3_accept_info info;
	struct socket *socket;
	struct file *endpoint;
	int fd, ret;
	if (cmd != NS3_PUBLISH_ACCEPT) return -ENOTTY;
	if (copy_from_user(&info, (void __user *)arg, sizeof(info))) return -EFAULT;
	if (info.local.family != listener->local.family || info.peer.family != listener->local.family)
		return -EAFNOSUPPORT;
	mutex_lock(&listener->lock);
	ret = listener->dead || listener->app_closed ? -ENETDOWN :
		!listener->listening ? -EINVAL :
		listener->accept_count >= listener->backlog ? -EAGAIN : 0;
	mutex_unlock(&listener->lock);
	if (ret) return ret;
	fd = get_unused_fd_flags(O_CLOEXEC);
	if (fd < 0) return fd;
	ret = sock_create_lite(listener->sk.sk_family, SOCK_STREAM, IPPROTO_TCP, &socket);
	if (ret) goto unused;
	ret = ns3_create(sock_net(&listener->sk), socket, IPPROTO_TCP, 1, listener->sk.sk_family);
	if (ret) { sock_release(socket); goto unused; }
	child = ns3_sk(socket);
	child->opened = child->connected = true;
	child->local = info.local; child->peer = info.peer;
	child->accept_socket = socket;
	info.socket = cpu_to_le64(child->id);
	if (copy_to_user((void __user *)arg, &info, sizeof(info))) { ret = -EFAULT; goto child; }
	sock_hold(&child->sk);
	endpoint = anon_inode_getfile("netstack3-endpoint", &ns3_endpoint_fops, child, O_RDWR | O_NONBLOCK);
	if (IS_ERR(endpoint)) { sock_put(&child->sk); ret = PTR_ERR(endpoint); goto child; }
	mutex_lock(&listener->lock);
	if (listener->dead || listener->app_closed || listener->accept_count >= listener->backlog) {
		mutex_unlock(&listener->lock); fput(endpoint); ret = -EAGAIN; goto child;
	}
	list_add_tail(&child->accept_node, &listener->accepted); listener->accept_count++;
	listener->sk.sk_data_ready(&listener->sk);
	fd_install(fd, endpoint);
	mutex_unlock(&listener->lock);
	return fd;
child:
	sock_release(socket);
unused:
	put_unused_fd(fd);
	return ret;
}
static const struct file_operations ns3_endpoint_fops = {
	.owner = THIS_MODULE, .read = ns3_read, .write = ns3_write,
	.poll = ns3_endpoint_poll, .release = ns3_endpoint_release,
	.unlocked_ioctl = ns3_publish_accept,
};
static __poll_t ns3_device_poll(struct file *f, poll_table *wait)
{
	struct ns3_session *session = f->private_data;
	struct ns3_net *n = net_generic(session->net, ns3_net_id);
	struct ns3_sock *s;
	__poll_t mask = 0;
	poll_wait(f, &n->wait, wait);
	mutex_lock(&n->lock);
	if (!n->online || session->generation != n->generation) mask = EPOLLERR | EPOLLHUP;
	else list_for_each_entry(s, &n->sockets, node) {
		if (!s->claimed && !s->dead) { mask = EPOLLIN; break; }
	}
	mutex_unlock(&n->lock);
	return mask;
}
/* Claim the next endpoint, like accept: the returned FD grants authority only
 * to that kernel socket. Failed user copies never consume a claim. */
static long ns3_claim(struct file *f, unsigned int cmd, unsigned long arg)
{
	struct ns3_session *session = f->private_data;
	struct ns3_net *n = net_generic(session->net, ns3_net_id);
	struct ns3_sock *s;
	struct file *endpoint;
	int fd, ret = -EAGAIN;
	if (cmd != NS3_CLAIM) return -ENOTTY;
	fd = get_unused_fd_flags(O_CLOEXEC);
	if (fd < 0) return fd;
	mutex_lock(&n->lock);
	if (!n->online || session->generation != n->generation) { ret = -ENETDOWN; goto out; }
	list_for_each_entry(s, &n->sockets, node) {
		__le64 id = cpu_to_le64(s->id);
		if (s->claimed || s->dead) continue;
		if (copy_to_user((void __user *)arg, &id, sizeof(id))) { ret = -EFAULT; break; }
		sock_hold(&s->sk);
		endpoint = anon_inode_getfile("netstack3-endpoint", &ns3_endpoint_fops, s, O_RDWR | O_NONBLOCK);
		if (IS_ERR(endpoint)) { sock_put(&s->sk); ret = PTR_ERR(endpoint); break; }
		s->claimed = true;
		fd_install(fd, endpoint);
		mutex_unlock(&n->lock);
		return fd;
	}
out:
	mutex_unlock(&n->lock);
	put_unused_fd(fd);
	return ret;
}
static int ns3_device_open(struct inode *inode, struct file *f)
{
	struct ns3_session *session;
	struct ns3_net *n;
	if (!ns_capable(current->nsproxy->net_ns->user_ns, CAP_NET_ADMIN)) return -EPERM;
	session = kzalloc(sizeof(*session), GFP_KERNEL);
	if (!session) return -ENOMEM;
	session->net = get_net(current->nsproxy->net_ns);
	n = net_generic(session->net, ns3_net_id);
	mutex_lock(&n->lock);
	if (n->online) { mutex_unlock(&n->lock); put_net(session->net); kfree(session); return -EBUSY; }
	session->generation = ++n->generation; n->online = true;
	mutex_unlock(&n->lock);
	f->private_data = session;
	return 0;
}
static int ns3_device_release(struct inode *inode, struct file *f)
{
	struct ns3_session *session = f->private_data;
	struct ns3_net *n = net_generic(session->net, ns3_net_id);
	mutex_lock(&n->lock);
	if (session->generation == n->generation) ns3_abort(n);
	mutex_unlock(&n->lock);
	put_net(session->net); kfree(session);
	return 0;
}
static const struct file_operations ns3_fops = {
	.owner = THIS_MODULE, .open = ns3_device_open, .release = ns3_device_release,
	.unlocked_ioctl = ns3_claim, .poll = ns3_device_poll,
};
static struct miscdevice ns3_device = {
	.minor = MISC_DYNAMIC_MINOR, .name = "netstack3", .mode = 0600, .fops = &ns3_fops,
};
static int __net_init ns3_net_init(struct net *net)
{
	struct ns3_net *n = net_generic(net, ns3_net_id);
	mutex_init(&n->lock); init_waitqueue_head(&n->wait);
	INIT_LIST_HEAD(&n->sockets);
	return 0;
}
static struct pernet_operations ns3_pernet = {
	.init = ns3_net_init, .id = &ns3_net_id, .size = sizeof(struct ns3_net),
};
static int __init ns3_init(void)
{
	int ret = proto_register(&ns3_proto, 1);
	if (ret) return ret;
	ret = register_pernet_subsys(&ns3_pernet);
	if (ret) goto proto;
	ret = sock_register(&ns3_family4);
	if (ret) goto pernet;
	ret = sock_register(&ns3_family6);
	if (ret) goto family4;
	ret = misc_register(&ns3_device);
	if (!ret) return 0;
	sock_unregister(PF_INET6);
family4:
	sock_unregister(PF_INET);
pernet:
	unregister_pernet_subsys(&ns3_pernet);
proto:
	proto_unregister(&ns3_proto);
	return ret;
}
subsys_initcall(ns3_init);
