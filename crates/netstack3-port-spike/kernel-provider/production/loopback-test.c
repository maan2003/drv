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
static void tcp(int family) {
	const size_t length = payload_length;
	struct timespec begin, end;
	clock_gettime(CLOCK_MONOTONIC, &begin);
	struct sockaddr_storage a = addr(family, 23456);
	int listener = socket(family, SOCK_STREAM | SOCK_CLOEXEC, 0);
	check(listener >= 0, "tcp socket");
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
	int client = socket(family, SOCK_DGRAM, 0);
	check(server >= 0 && client >= 0, "udp sockets");
	check(bind(server, (void *)&a, alen(family)) == 0, "udp bind");
	char b[64]; socklen_t n = sizeof(source);
	check(recv(server, b, sizeof(b), 0) < 0 && errno == EAGAIN, "empty nonblocking UDP");
	check(sendto(client, "", 0, 0, (void *)&a, alen(family)) == 0, "zero datagram send");
	wait_event(server, EPOLLIN);
	check(recvfrom(server, b, sizeof(b), 0, (void *)&source, &n) == 0, "zero datagram receive");
	check(source.ss_family == family, "datagram source family");
	check(sendto(client, "abcdef", 6, 0, (void *)&a, alen(family)) == 6, "udp data send");
	wait_event(server, EPOLLIN);
	check(recv(server, b, 2, MSG_PEEK) == 2 && !memcmp(b,"ab",2), "UDP peek");
	check(recv(server, b, 2, MSG_TRUNC) == 6, "UDP truncation original length");
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
int main(int argc, char **argv) {
	setbuf(stdout, NULL); alarm(90);
	if (argc == 2 && !strcmp(argv[1], "bench")) { payload_length = 8 * 1024 * 1024; tcp(AF_INET); tcp(AF_INET6); return 0; }
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
    refused(AF_INET); refused(AF_INET6);
	tcp(AF_INET); udp(AF_INET); tcp(AF_INET6); udp(AF_INET6);
	puts("PASS LOOPBACK_SUITE"); return 0;
}
