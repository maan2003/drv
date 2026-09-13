// SPDX-License-Identifier: GPL-2.0-only
#define _GNU_SOURCE
#include <sys/socket.h>
#include <sys/epoll.h>
#include <sys/wait.h>
#include <arpa/inet.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <time.h>
static size_t payload_length = 1024 * 1024 + 137;
static void check(int ok, const char *what) {
	if (!ok) { fprintf(stderr, "FAIL %s: %s\n", what, strerror(errno)); exit(1); }
}
static struct sockaddr_storage addr(int family, unsigned short port) {
	struct sockaddr_storage a = {};
	if (family == AF_INET) {
		struct sockaddr_in *v = (void *)&a;
		v->sin_family = family; v->sin_port = htons(port);
		inet_pton(family, "127.0.0.1", &v->sin_addr);
	} else {
		struct sockaddr_in6 *v = (void *)&a;
		v->sin6_family = family; v->sin6_port = htons(port);
		inet_pton(family, "::1", &v->sin6_addr);
	}
	return a;
}
static socklen_t alen(int family) { return family == AF_INET ? sizeof(struct sockaddr_in) : sizeof(struct sockaddr_in6); }
static void check_name(int fd, int family, int peer, unsigned short expected_port, int wildcard) {
    struct sockaddr_storage got;
    socklen_t n = sizeof(got);
    check((peer ? getpeername(fd, (void *)&got, &n) : getsockname(fd, (void *)&got, &n)) == 0,
          peer ? "getpeername" : "getsockname");
    struct sockaddr_storage expected = addr(family, expected_port);
    if (wildcard) {
        if (family == AF_INET) ((struct sockaddr_in *)&expected)->sin_addr.s_addr = 0;
        else memset(&((struct sockaddr_in6 *)&expected)->sin6_addr, 0, 16);
    }
    check(n == alen(family) && got.ss_family == family, "socket name family");
    if (family == AF_INET) {
        struct sockaddr_in *g = (void *)&got, *e = (void *)&expected;
        check(g->sin_addr.s_addr == e->sin_addr.s_addr, "core-selected IPv4 socket name");
        check(expected_port ? g->sin_port == e->sin_port : (wildcard ? !g->sin_port : !!g->sin_port), "IPv4 socket name port");
    } else {
        struct sockaddr_in6 *g = (void *)&got, *e = (void *)&expected;
        check(!memcmp(&g->sin6_addr, &e->sin6_addr, 16), "core-selected IPv6 socket name");
        check(expected_port ? g->sin6_port == e->sin6_port : (wildcard ? !g->sin6_port : !!g->sin6_port), "IPv6 socket name port");
    }
}
static void wait_event(int fd, unsigned int mask) {
	int ep = epoll_create1(EPOLL_CLOEXEC);
	struct epoll_event e = { .events = mask }, got;
	check(ep >= 0, "epoll_create");
	check(epoll_ctl(ep, EPOLL_CTL_ADD, fd, &e) == 0, "epoll add");
	check(epoll_wait(ep, &got, 1, 5000) == 1, "epoll wait");
	check((got.events & mask) != 0, "epoll requested readiness");
	close(ep);
}
static void transfer(int fd, unsigned char *buf, size_t len, int sending) {
	size_t done = 0;
	while (done < len) {
		ssize_t n = sending ? send(fd, buf + done, len - done, MSG_NOSIGNAL) :
			recv(fd, buf + done, len - done, 0);
		if (n < 0 && (errno == EAGAIN || errno == EINTR)) {
			wait_event(fd, sending ? EPOLLOUT : EPOLLIN); continue;
		}
		check(n > 0, sending ? "stream send" : "stream receive");
		done += n;
	}
}
static void tcp(int family, unsigned short port) {
	const size_t length = payload_length;
	struct timespec begin, end;
	clock_gettime(CLOCK_MONOTONIC, &begin);
	struct sockaddr_storage a = addr(family, port);
	int listener = socket(family, SOCK_STREAM | SOCK_CLOEXEC, 0);
	check(listener >= 0, "tcp socket");
    check_name(listener, family, 0, 0, 1);
	check(bind(listener, (void *)&a, alen(family)) == 0, "tcp bind");
	check(listen(listener, 8) == 0, "tcp listen");
    check(fcntl(listener, F_SETFL, O_NONBLOCK) == 0, "listener nonblock");
    check(accept4(listener, NULL, NULL, 0) < 0 && errno == EAGAIN, "empty accept nonblock");
    check(fcntl(listener, F_SETFL, 0) == 0, "listener blocking");
	pid_t child = fork();
	check(child >= 0, "fork");
	if (!child) {
		close(listener);
		int fd = socket(family, SOCK_STREAM | SOCK_NONBLOCK, 0);
		check(fd >= 0, "client socket");
		int r = connect(fd, (void *)&a, alen(family));
		check(r == 0 || (r == -1 && errno == EINPROGRESS), "nonblocking connect");
		wait_event(fd, EPOLLOUT);
		int error = -1; socklen_t n = sizeof(error);
		check(getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &n) == 0 && error == 0, "connect SO_ERROR");
        check_name(fd, family, 0, 0, 0);
        check_name(fd, family, 1, port, 0);
		unsigned char *buf = malloc(length);
		check(buf != NULL, "client malloc");
		for (size_t i = 0; i < length; ++i) buf[i] = (i * 29 + i / 251) & 255;
		transfer(fd, buf, length, 1);
		check(shutdown(fd, SHUT_WR) == 0, "client half close");
		memset(buf, 0, length);
		transfer(fd, buf, length, 0);
		for (size_t i = 0; i < length; ++i) check(buf[i] == ((i * 29 + i / 251) & 255), "client payload integrity");
		wait_event(fd, EPOLLIN);
		check(recv(fd, buf, 1, 0) == 0, "client EOF");
		close(fd); free(buf); _exit(0);
	}
	int fd = accept4(listener, NULL, NULL, SOCK_CLOEXEC);
	check(fd >= 0, "accept");
	unsigned char *buf = malloc(length);
	check(buf != NULL, "server malloc");
	transfer(fd, buf, length, 0);
	for (size_t i = 0; i < length; ++i) check(buf[i] == ((i * 29 + i / 251) & 255), "server payload integrity");
	unsigned char byte;
	check(recv(fd, &byte, 1, 0) == 0, "server half-close EOF");
	/* fd sharing retains one socket until final file reference closes. */
	int dupfd = dup(fd); check(dupfd >= 0, "dup"); close(fd);
	transfer(dupfd, buf, length, 1);
	check(shutdown(dupfd, SHUT_WR) == 0, "server half close");
	close(dupfd); close(listener); free(buf);
	int status; check(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0, "client process success");
	clock_gettime(CLOCK_MONOTONIC, &end);
	double seconds = end.tv_sec - begin.tv_sec + (end.tv_nsec - begin.tv_nsec) / 1e9;
	printf("BENCH TCP family=%d seconds=%.6f MBps=%.2f\n", family, seconds, length * 2.0 / seconds / 1e6);
	printf("PASS TCP family=%d bytes=%zu bidirectional integrity epoll nonblock halfclose dup fork\n", family, length); fflush(stdout);
}
static void udp(int family) {
	struct sockaddr_storage a = addr(family, 23457), source;
	int server = socket(family, SOCK_DGRAM | SOCK_NONBLOCK, 0);
	int client = socket(family, SOCK_DGRAM | SOCK_NONBLOCK, 0);
	check(server >= 0 && client >= 0, "udp sockets");
	check(bind(server, (void *)&a, alen(family)) == 0, "udp bind");
	char b[64]; socklen_t n = sizeof(source);
	check(recv(server, b, sizeof(b), 0) < 0 && errno == EAGAIN, "empty nonblocking UDP");
	ssize_t sent = sendto(client, "", 0, 0, (void *)&a, alen(family));
    if (sent < 0 && errno == EAGAIN) {
        /* Lazy activation consumes no payload; retry after committed metadata. */
        wait_event(client, EPOLLOUT);
        sent = sendto(client, "", 0, 0, (void *)&a, alen(family));
    }
    check(sent == 0, "zero datagram send");
	wait_event(server, EPOLLIN);
	check(recvfrom(server, b, sizeof(b), 0, (void *)&source, &n) == 0, "zero datagram receive");
	check(source.ss_family == family, "datagram source family");
	check(sendto(client, "abcdef", 6, 0, (void *)&a, alen(family)) == 6, "udp data send");
	wait_event(server, EPOLLIN);
	check(recv(server, b, 2, MSG_PEEK) == 2 && !memcmp(b,"ab",2), "UDP peek");
	check(recv(server, b, 2, MSG_TRUNC) == 6, "UDP truncation original length");
	int enabled = 1;
	check(setsockopt(client, family == AF_INET ? IPPROTO_IP : IPPROTO_IPV6,
		family == AF_INET ? IP_RECVERR : IPV6_RECVERR, &enabled, sizeof(enabled)) < 0 &&
		errno == ENOPROTOOPT, "unsupported optional protocol option");
	check(connect(client, (void *)&a, alen(family)) == 0, "nonblocking UDP connect is immediate");
    check_name(client, family, 0, 0, 0);
    check_name(client, family, 1, 23457, 0);
	check(send(client, "peer", 4, 0) == 4, "connected UDP send");
	wait_event(server, EPOLLIN);
	check(recv(server, b, sizeof(b), 0) == 4 && !memcmp(b, "peer", 4), "connected UDP receive");

	close(client); close(server);
	printf("PASS UDP family=%d zero-datagram source peek truncation nonblock\n", family); fflush(stdout);
}
static void refused(int family) {
    struct sockaddr_storage a = addr(family, 23458);
    int fd = socket(family, SOCK_STREAM | SOCK_NONBLOCK, 0);
    check(fd >= 0, "refused socket");
    check(connect(fd, (void *)&a, alen(family)) < 0 && errno == EINPROGRESS, "refused connect pending");
    wait_event(fd, EPOLLERR);
    int error = 0; socklen_t size = sizeof(error);
    check(getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &size) == 0 && error == ECONNREFUSED, "refused SO_ERROR");
    check(getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &size) == 0 && error == 0, "SO_ERROR clears");
    close(fd);
    printf("PASS REFUSED family=%d SO_ERROR clears\n", family);
}

/* Linux adds MSG_BATCH internally for all but the last sendmmsg element.
 * The frontend must accept that scheduling hint without changing datagrams. */
static void udp_batch(int family) {
    int rx = socket(family, SOCK_DGRAM, 0);
    int tx = socket(family, SOCK_DGRAM, 0);
    struct sockaddr_storage a = addr(family, 0);
    socklen_t size = alen(family);
    check(rx >= 0 && tx >= 0, "batch UDP sockets");
    check(bind(rx, (void *)&a, size) == 0, "batch UDP bind");
    check(getsockname(rx, (void *)&a, &size) == 0, "batch UDP name");
    for (unsigned int count = 1; count <= 8; count *= 2) {
        struct mmsghdr messages[8] = {};
        struct iovec iov[8];
        unsigned char payload[8][17];
        for (unsigned int i = 0; i < count; ++i) {
            memset(payload[i], i + count, sizeof(payload[i]));
            iov[i] = (struct iovec){ .iov_base = payload[i], .iov_len = i + 1 };
            messages[i].msg_hdr = (struct msghdr){
                .msg_name = &a, .msg_namelen = size, .msg_iov = &iov[i], .msg_iovlen = 1
            };
        }
        check(sendmmsg(tx, messages, count, MSG_NOSIGNAL) == (int)count,
              "sendmmsg batch accepted");
        for (unsigned int i = 0; i < count; ++i) {
            unsigned char received[17];
            check(messages[i].msg_len == i + 1, "sendmmsg per-message length");
            check(recv(rx, received, sizeof(received), 0) == (ssize_t)i + 1,
                  "batch datagram boundaries");
            check(!memcmp(received, payload[i], i + 1), "batch datagram bytes");
        }
    }
    close(tx); close(rx);
    puts(family == AF_INET ? "PASS SENDMMSG_UDP_V4" : "PASS SENDMMSG_UDP_V6");
}

/* The reader cannot release TCP window space until shutdown has returned.
 * Fill beyond both core and frontend buffering before imposing the barrier. */
static void shutdown_backpressure(int family) {
    struct sockaddr_storage a = addr(family, 24567);
    int listener = socket(family, SOCK_STREAM, 0), gate[2];
    check(listener >= 0 && pipe(gate) == 0, "shutdown setup");
    check(bind(listener, (void *)&a, alen(family)) == 0 &&
          listen(listener, 1) == 0, "shutdown listen");
    pid_t child = fork();
    check(child >= 0, "shutdown fork");
    if (!child) {
        close(gate[1]);
        int fd = accept(listener, NULL, NULL);
        check(fd >= 0, "shutdown accept");
        size_t expected, done = 0;
        check(read(gate[0], &expected, sizeof(expected)) == sizeof(expected), "shutdown barrier released");
        unsigned char bytes[16384];
        ssize_t n;
        while ((n = recv(fd, bytes, sizeof(bytes), 0)) > 0) {
            for (ssize_t i = 0; i < n; ++i)
                check(bytes[i] == ((done + i) % 251), "shutdown retained byte");
            done += n;
        }
        check(n == 0 && done == expected, "shutdown bytes before EOF");
        close(fd); close(listener); close(gate[0]); _exit(0);
    }
    close(gate[0]);
    int fd = socket(family, SOCK_STREAM, 0);
    check(fd >= 0 && connect(fd, (void *)&a, alen(family)) == 0, "shutdown connect");
    unsigned char bytes[16384];
    size_t total = 0;
    /* A blocking 2 MiB send cannot complete with the reader gated. Nonblocking
     * retries give the worker time to fill the peer window, then leave admitted
     * data behind it. The timeout is only a test guard, not a correctness oracle. */
    int stalled = 0;
    while (stalled < 100) {
        for (size_t i = 0; i < sizeof(bytes); ++i) bytes[i] = (total + i) % 251;
        ssize_t n = send(fd, bytes, sizeof(bytes), MSG_DONTWAIT | MSG_NOSIGNAL);
        if (n < 0 && errno == EAGAIN) { usleep(1000); ++stalled; continue; }
        check(n > 0, "shutdown admission");
        total += n; stalled = 0;
        check(total < 16 * 1024 * 1024, "bounded shutdown buffering");
    }
    check(shutdown(fd, SHUT_WR) == 0, "shutdown without reader progress");
    check(write(gate[1], &total, sizeof(total)) == sizeof(total), "release shutdown reader");
    int status;
    check(waitpid(child, &status, 0) == child && WIFEXITED(status) &&
          WEXITSTATUS(status) == 0, "shutdown reader status");
    close(fd); close(listener); close(gate[1]);
    printf("PASS SHUTDOWN_BACKPRESSURE_V%d bytes=%zu\n", family == AF_INET ? 4 : 6, total);
}

int main(int argc, char **argv) {
	setbuf(stdout, NULL); alarm(90);
    if (argc == 2 && !strcmp(argv[1], "batch")) { udp_batch(AF_INET); udp_batch(AF_INET6); return 0; }
    if (argc == 2 && !strcmp(argv[1], "parallel")) {
        enum { CLIENTS = 16 };
        pid_t children[CLIENTS];
        payload_length = 512 * 1024;
        for (int i = 0; i < CLIENTS; i++) {
            children[i] = fork();
            check(children[i] >= 0, "parallel fork");
            if (!children[i]) {
                tcp(i % 2 ? AF_INET6 : AF_INET, 24000 + i);
                _exit(0);
            }
        }
        for (int i = 0; i < CLIENTS; i++) {
            int status;
            check(waitpid(children[i], &status, 0) == children[i] &&
                WIFEXITED(status) && WEXITSTATUS(status) == 0, "parallel client");
        }
        puts("PASS PARALLEL_TCP_16");
        return 0;
    }
    if (argc == 2 && !strcmp(argv[1], "bench-idle")) {
        enum { IDLE = 128 };
        int idle[IDLE];
        for (int i = 0; i < IDLE; ++i) {
            idle[i] = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
            struct sockaddr_storage a = addr(AF_INET, 0);
            check(idle[i] >= 0 && bind(idle[i], (void *)&a, alen(AF_INET)) == 0,
                  "idle UDP ephemeral bind");
        }
        payload_length = 64 * 1024 * 1024;
        puts("BENCH_IDLE sockets=128");
        tcp(AF_INET, 23456); tcp(AF_INET6, 23456);
        for (int i = 0; i < IDLE; ++i) close(idle[i]);
        puts("PASS IDLE_SOCKET_TCP");
        return 0;
    }
	if (argc == 2 && (!strcmp(argv[1], "bench") || !strcmp(argv[1], "bench-long"))) {
        payload_length = (!strcmp(argv[1], "bench-long") ? 64 : 8) * 1024 * 1024;
        tcp(AF_INET, 23456); tcp(AF_INET6, 23456); return 0;
    }
	if (argc == 2 && !strcmp(argv[1], "absent")) {
		int fd = socket(AF_INET, SOCK_STREAM, 0);
		check(fd < 0 && errno == ENETDOWN, "provider absent fails closed");
		puts("PASS PROVIDER_ABSENT_NO_NATIVE_FALLBACK"); return 0;
	}
    if (argc == 2 && !strcmp(argv[1], "death")) {
        int fd = socket(AF_INET, SOCK_DGRAM, 0);
        struct sockaddr_storage a = addr(AF_INET, 23459);
        check(fd >= 0 && bind(fd, (void *)&a, alen(AF_INET)) == 0, "death bind");
        puts("DEATH_READY");
        char byte;
        check(recv(fd, &byte, 1, 0) < 0 && errno == ENETDOWN, "provider death wakes blocked read");
        wait_event(fd, EPOLLHUP);
        sleep(2); /* replacement must not resurrect this socket */
        check(sendto(fd, "x", 1, 0, (void *)&a, alen(AF_INET)) < 0 && errno == ENETDOWN, "old generation remains dead");
        close(fd);
        puts("PASS PROVIDER_DEATH_WAKE_AND_NO_RESURRECTION");
        return 0;
    }
    shutdown_backpressure(AF_INET); shutdown_backpressure(AF_INET6);
    udp_batch(AF_INET); udp_batch(AF_INET6);
    refused(AF_INET); refused(AF_INET6);
	tcp(AF_INET, 23456); udp(AF_INET); tcp(AF_INET6, 23456); udp(AF_INET6);
	puts("PASS LOOPBACK_SUITE"); return 0;
}
