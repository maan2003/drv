// SPDX-License-Identifier: GPL-2.0-only
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <unistd.h>

#define DEV "/dev/netstack3-provider"
#define MAGIC 0x5033534eU
#define MAX_FRAME (40 + 65536)
#define RESPONSE 2
#define EVENT 3
#define OPEN_CLIENT 1
#define OPEN_SOCKET 3
#define BIND 4
#define CONNECT 5
#define LISTEN 7
#define ACCEPT 8
#define SENDMSG_OP 9
#define RECVMSG_OP 10
#define SHUTDOWN 11
#define GETNAME 12
#define GET_ERROR 13
#define CLOSE 17
#define READY_CHANGED 18

struct __attribute__((packed)) header {
	uint32_t magic;
	uint16_t version, type, opcode, reserved;
	uint64_t namespace_id, client_id, request_id;
	uint32_t payload_len;
};

static int provider_fd;
static atomic_int requests, closes, failures, stop_provider, shutdowns;
static atomic_ullong next_handle = 40;

static void fail(const char *what)
{
	fprintf(stderr, "not ok - %s: %s\n", what, strerror(errno));
	atomic_fetch_add(&failures, 1);
}

static int write_frame(const struct header *request, uint16_t type,
		       uint16_t opcode, const void *payload, uint32_t len)
{
	uint8_t frame[MAX_FRAME];
	struct header *h = (void *)frame;
	*h = *request;
	h->type = type;
	h->opcode = opcode;
	h->payload_len = len;
	if (len)
		memcpy(frame + sizeof(*h), payload, len);
	return write(provider_fd, frame, sizeof(*h) + len) == (ssize_t)(sizeof(*h) + len) ? 0 : -1;
}

static void respond(const struct header *h, const void *result, uint32_t len)
{
	uint8_t payload[65536];
	payload[0] = 0;
	if (len)
		memcpy(payload + 1, result, len);
	if (write_frame(h, RESPONSE, h->opcode, payload, len + 1))
		fail("provider response");
}

static void respond_error(const struct header *h, uint8_t code)
{
	uint8_t payload[] = { 1, code };
	if (write_frame(h, RESPONSE, h->opcode, payload, sizeof(payload)))
		fail("provider error response");
}

static void send_ready(const struct header *h, uint64_t handle, uint16_t mask)
{
	uint8_t payload[19] = { 0 };
	uint64_t sequence = 1;
	memcpy(payload, &sequence, 8);
	memcpy(payload + 8, &handle, 8);
	memcpy(payload + 16, &mask, 2);
	if (write_frame(h, EVENT, READY_CHANGED, payload, sizeof(payload)))
		fail("readiness event");
}

static void *provider(void *unused)
{
	uint8_t frame[MAX_FRAME];
	int recv_count = 0;
	(void)unused;
	while (!atomic_load(&stop_provider)) {
		ssize_t n = read(provider_fd, frame, sizeof(frame));
		struct header *h = (void *)frame;
		uint8_t *p = frame + sizeof(*h), result[64] = { 0 };
		uint64_t handle;
		uint32_t sent;
		if (n < 0) {
			if (errno == EINTR) continue;
			break;
		}
		if (n < (ssize_t)sizeof(*h) || h->magic != MAGIC || h->version != 2 ||
		    h->reserved || n != (ssize_t)(sizeof(*h) + h->payload_len)) {
			errno = EPROTO; fail("request framing"); break;
		}
		if (!h->namespace_id || !h->client_id) { errno = EPROTO; fail("immutable identity"); }
		atomic_fetch_add(&requests, 1);
		switch (h->opcode) {
		case OPEN_CLIENT:
			respond(h, NULL, 0); break;
		case OPEN_SOCKET:
			handle = atomic_fetch_add(&next_handle, 1);
			respond(h, &handle, 8); break;
		case BIND:
			/* Return the requested explicit address, including port zero. */
			respond(h, p + 8, h->payload_len - 8); break;
		case CONNECT:
			respond(h, NULL, 0);
			memcpy(&handle, p, 8);
			send_ready(h, handle, (1u << 0) | (1u << 1) | (1u << 6));
			break;
		case LISTEN:
			result[0] = 4; result[1] = 192; result[2] = 0; result[3] = 2;
			result[4] = 1; result[5] = 0x34; result[6] = 0x12;
			respond(h, result, 7);
			memcpy(&handle, p, 8); send_ready(h, handle, 1u << 2); break;
		case ACCEPT:
			handle = atomic_fetch_add(&next_handle, 1);
			memcpy(result, &handle, 8);
			result[8] = result[15] = 4;
			result[9] = 192; result[10] = 0; result[11] = 2; result[12] = 1;
			result[13] = 0x34; result[14] = 0x12;
			result[16] = 198; result[17] = 51; result[18] = 100; result[19] = 2;
			result[20] = 0x78; result[21] = 0x56;
			respond(h, result, 22); break;
		case SENDMSG_OP:
			sent = h->payload_len - 13;
			respond(h, &sent, 4); break;
		case RECVMSG_OP:
			if (recv_count++ == 0) {
				/* Successful zero-length UDP datagram with source address. */
				result[9] = 4; result[10] = 198; result[11] = 51;
				result[12] = 100; result[13] = 9;
				result[14] = 0x35; result[15] = 0;
				respond(h, result, 16);
			} else {
				respond_error(h, 7); /* WouldBlock, distinct from empty. */
			}
			break;
		case SHUTDOWN: case GETNAME:
			if (h->opcode == GETNAME) {
				result[0] = 4; result[1] = 192; result[2] = 0;
				result[3] = 2; result[4] = 1;
				respond(h, result, 7);
			} else { if (h->payload_len != 9 || p[8] < 1 || p[8] > 3) { errno = EPROTO; fail("directional shutdown"); } atomic_fetch_add(&shutdowns, 1); respond(h, NULL, 0); }
			break;
		case GET_ERROR:
			respond(h, result, 1); break;
		case CLOSE:
			atomic_fetch_add(&closes, 1); respond(h, NULL, 0); break;
		default:
			respond_error(h, 17); break;
		}
	}
	return NULL;
}

static int loopback4(void)
{
	struct sockaddr_in a = { .sin_family = AF_INET, .sin_port = 0 };
	char c = 'x', out = 0; socklen_t alen = sizeof(a);
	int before = atomic_load(&requests), s = socket(AF_INET, SOCK_DGRAM, 0);
	inet_pton(AF_INET, "127.23.45.67", &a.sin_addr);
	if (s < 0 || bind(s, (void *)&a, sizeof(a)) || getsockname(s, (void *)&a, &alen) ||
	    connect(s, (void *)&a, sizeof(a)) || send(s, &c, 1, 0) != 1 || recv(s, &out, 1, 0) != 1 || out != c) {
		fail("IPv4 127/8 native loopback"); if (s >= 0) close(s); return -1;
	}
	close(s);
	if (before != atomic_load(&requests)) { errno = EPROTO; fail("127/8 bypassed provider"); return -1; }
	return 0;
}

static int loopback6(void)
{
	struct sockaddr_in6 a = { .sin6_family = AF_INET6, .sin6_addr = IN6ADDR_LOOPBACK_INIT };
	char c = 'y', out = 0; socklen_t alen = sizeof(a);
	int before = atomic_load(&requests), s = socket(AF_INET6, SOCK_DGRAM, 0);
	if (s < 0 || bind(s, (void *)&a, sizeof(a)) || getsockname(s, (void *)&a, &alen) ||
	    connect(s, (void *)&a, sizeof(a)) || send(s, &c, 1, 0) != 1 || recv(s, &out, 1, 0) != 1 || out != c) {
		fail("IPv6 ::1 native loopback"); if (s >= 0) close(s); return -1;
	}
	close(s);
	if (before != atomic_load(&requests)) { errno = EPROTO; fail("::1 bypassed provider"); return -1; }
	return 0;
}

static int remote_udp(void)
{
	struct sockaddr_in a = { .sin_family = AF_INET, .sin_port = htons(53) }, from;
	char byte = 0; socklen_t fromlen = sizeof(from);
	int ep, s = socket(AF_INET, SOCK_DGRAM | SOCK_NONBLOCK, 0), duplicate;
	struct epoll_event ev = { .events = EPOLLIN | EPOLLOUT | EPOLLET, .data.fd = s }, got;
	inet_pton(AF_INET, "192.0.2.53", &a.sin_addr);
	if (s < 0 || connect(s, (void *)&a, sizeof(a))) { fail("remote UDP connect"); return -1; }
	ep = epoll_create1(EPOLL_CLOEXEC);
	if (ep < 0 || epoll_ctl(ep, EPOLL_CTL_ADD, s, &ev) || epoll_wait(ep, &got, 1, 1000) != 1) {
		fail("remote epoll readiness"); return -1;
	}
	if (send(s, "", 0, 0) != 0) { fail("zero UDP send boundary"); return -1; }
	if (recvfrom(s, &byte, sizeof(byte), 0, (void *)&from, &fromlen) != 0 ||
	    from.sin_family != AF_INET || ntohl(from.sin_addr.s_addr) != 0xc6336409) {
		fail("zero UDP receive and peer identity"); return -1;
	}
	if (recv(s, &byte, 1, 0) != -1 || errno != EAGAIN) {
		errno = EPROTO; fail("no datagram differs from zero datagram"); return -1;
	}
	duplicate = dup(s);
	close(s);
	if (duplicate < 0) { fail("dup socket fd"); return -1; }
	{ int before = atomic_load(&closes); close(duplicate);
	  if (atomic_load(&closes) != before + 1) { errno = EPROTO; fail("close only on final fd"); return -1; } }
	close(ep);
	return 0;
}

static int remote_tcp(void)
{
	struct sockaddr_in a = { .sin_family = AF_INET, .sin_port = 0 }, name;
	socklen_t nlen = sizeof(name);
	int listener = socket(AF_INET, SOCK_STREAM, 0), child, connected, ep;
	struct epoll_event ev = { .events = EPOLLIN, .data.fd = listener }, got;
	inet_pton(AF_INET, "192.0.2.1", &a.sin_addr);
	if (listener < 0 || bind(listener, (void *)&a, sizeof(a)) ||
	    listen(listener, 0) || getsockname(listener, (void *)&name, &nlen)) {
		fail("remote TCP bind/listen/getname"); return -1;
	}
	ep = epoll_create1(EPOLL_CLOEXEC);
	if (ep < 0 || epoll_ctl(ep, EPOLL_CTL_ADD, listener, &ev) ||
	    epoll_wait(ep, &got, 1, 1000) != 1) { fail("listener incoming readiness"); return -1; }
	child = accept4(listener, NULL, NULL, SOCK_CLOEXEC);
	if (child < 0) { fail("remote TCP accept"); return -1; }
	close(child); close(listener); close(ep);
	connected = socket(AF_INET, SOCK_STREAM, 0); a.sin_port = htons(443);
	if (connected < 0 || connect(connected, (void *)&a, sizeof(a)) ||
	    send(connected, "abc", 3, 0) != 3 || shutdown(connected, SHUT_WR)) {
		fail("remote TCP connect/write/shutdown"); return -1;
	}
	return connected;
}

static int wildcard_rejected(void)
{
	struct sockaddr_in any = { .sin_family = AF_INET };
	int s = socket(AF_INET, SOCK_STREAM, 0), ret = bind(s, (void *)&any, sizeof(any));
	close(s);
	if (ret != -1 || errno != EOPNOTSUPP) { errno = EPROTO; fail("conservative wildcard policy"); return -1; }
	return 0;
}

int main(void)
{
	pthread_t thread;
	int remote, loop, live_tcp;
	printf("TAP version 13\n1..10\n");
	provider_fd = open(DEV, O_RDWR | O_CLOEXEC);
	if (provider_fd < 0) { fail("open provider device"); return 1; }
	if (pthread_create(&thread, NULL, provider, NULL)) { fail("provider thread"); return 1; }
	loop = loopback4(); printf("%s 1 - all IPv4 127/8 remains Linux loopback\n", loop ? "not ok" : "ok");
	loop = loopback6(); printf("%s 2 - IPv6 ::1 remains Linux loopback\n", loop ? "not ok" : "ok");
	remote = remote_udp(); printf("%s 3 - UDP connect, peer, empty datagram and fd lifecycle\n", remote ? "not ok" : "ok");
	live_tcp = remote_tcp(); printf("%s 4 - TCP bind/listen/accept/connect/shutdown and readiness\n", live_tcp < 0 ? "not ok" : "ok");
	remote = wildcard_rejected(); printf("%s 5 - wildcard bind is conservatively rejected\n", remote ? "not ok" : "ok");
	{ struct header bad = { .magic = MAGIC, .version = 2, .type = EVENT,
		.opcode = READY_CHANGED, .reserved = 1 };
	  int r = write(provider_fd, &bad, sizeof(bad));
	  printf("%s 6 - malformed frame rejected\n", r < 0 && errno == EPROTO ? "ok" : "not ok"); }
	atomic_store(&stop_provider, 1);
	pthread_cancel(thread); pthread_join(thread, NULL); close(provider_fd);
	{ struct epoll_event ev = { .events = EPOLLIN | EPOLLOUT, .data.fd = live_tcp }, got;
	  int ep = epoll_create1(EPOLL_CLOEXEC); epoll_ctl(ep, EPOLL_CTL_ADD, live_tcp, &ev);
	  int r = epoll_wait(ep, &got, 1, 1000);
	  printf("%s 7 - daemon death wakes epoll with HUP/ERR\n", r == 1 && (got.events & (EPOLLHUP | EPOLLERR)) ? "ok" : "not ok");
	  close(ep); close(live_tcp); }
	{ struct sockaddr_in a = { .sin_family = AF_INET, .sin_port = htons(9) };
	  int s = socket(AF_INET, SOCK_STREAM, 0), saved; inet_pton(AF_INET, "192.0.2.1", &a.sin_addr);
	  int r = connect(s, (void *)&a, sizeof(a)); saved = errno; close(s);
	  printf("%s 8 - provider absence fails remote closed\n", r < 0 && saved == ENETDOWN ? "ok" : "not ok"); }
	printf("%s 9 - immutable namespace/client identity validated\n", atomic_load(&failures) ? "not ok" : "ok");
	printf("%s 10 - directional shutdown reached provider\n", atomic_load(&shutdowns) ? "ok" : "not ok");
	return atomic_load(&failures) ? 1 : 0;
}
