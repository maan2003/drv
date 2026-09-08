// SPDX-License-Identifier: MIT OR Apache-2.0
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

static void marker(const char *path)
{
	int fd = open(path, O_WRONLY | O_CREAT | O_CLOEXEC, 0600);
	if (fd < 0) { perror("marker"); exit(1); }
	close(fd);
}

static void wait_for(const char *path)
{
	for (int i = 0; i < 300 && access(path, F_OK); i++) usleep(100000);
	if (access(path, F_OK)) { fprintf(stderr, "timed out waiting for %s\n", path); exit(1); }
}

static int recv_with_timeout(int fd, void *buffer, size_t length, int timeout_ms)
{
	struct pollfd p = { .fd = fd, .events = POLLIN };
	int ready = poll(&p, 1, timeout_ms);
	if (ready <= 0 || !(p.revents & POLLIN)) {
		errno = ready == 0 ? ETIMEDOUT : errno;
		return -1;
	}
	return recv(fd, buffer, length, 0);
}

static int connect_to(int type, uint16_t port)
{
	struct sockaddr_in address = { .sin_family = AF_INET, .sin_port = htons(port) };
	int fd = socket(AF_INET, type, 0);
	inet_pton(AF_INET, "192.0.2.2", &address.sin_addr);
	if (fd < 0 || connect(fd, (void *)&address, sizeof(address))) {
		perror("connect"); exit(1);
	}
	return fd;
}

int main(int argc, char **argv)
{
	const char *message;
	char reply[16];
	int fd, n;
	if (argc != 2) return 2;
	if (!strcmp(argv[1], "offline")) {
		struct sockaddr_in any = { .sin_family = AF_INET };
		int udp = socket(AF_INET, SOCK_DGRAM, 0), tcp = socket(AF_INET, SOCK_STREAM, 0);
		if (udp < 0 || tcp < 0) { perror("offline socket"); return 1; }
		if (bind(udp, (void *)&any, sizeof(any))) { perror("offline UDP bind"); return 1; }
		if (bind(tcp, (void *)&any, sizeof(any))) { perror("offline TCP bind"); return 1; }
		if (listen(tcp, 1)) { perror("offline TCP listen"); return 1; }
		fd = connect_to(SOCK_DGRAM, 9000);
		errno = 0;
		if (send(fd, "offline", 7, 0) >= 0 ||
		    (errno != ENETUNREACH && errno != EHOSTUNREACH && errno != EADDRNOTAVAIL)) {
			perror("offline UDP send did not return a reachability error");
			return 1;
		}
		close(fd); close(udp); close(tcp); return 0;
	} else if (!strcmp(argv[1], "udp") || !strcmp(argv[1], "preconfig") ||
		   !strcmp(argv[1], "survive") || !strcmp(argv[1], "keep")) {
		message = "udp-echo"; fd = connect_to(SOCK_DGRAM, 9000);
	} else {
		message = "tcp-echo"; fd = connect_to(SOCK_STREAM, 9001);
	}
	if (!strcmp(argv[1], "hold")) {
		struct pollfd pollfd = { .fd = fd, .events = POLLIN | POLLOUT };
		marker("/run/netstack3-test-live-socket");
		for (;;) {
			n = poll(&pollfd, 1, 5000);
			if (n > 0 && (pollfd.revents & (POLLERR | POLLHUP))) return 0;
			if (n < 0 && errno != EINTR) return 1;
		}
	}
	if (!strcmp(argv[1], "preconfig")) {
		marker("/run/netstack3-test-preconfig-ready");
		wait_for("/run/netstack3-test-use-preconfig");
		for (int i = 0; i < 100; i++) {
			errno = 0;
			if (send(fd, message, strlen(message), 0) == (ssize_t)strlen(message) &&
			    (n = recv_with_timeout(fd, reply, sizeof(reply), 200)) ==
				    (int)strlen(message) &&
			    !memcmp(reply, message, n)) {
				marker("/run/netstack3-test-preconfig-ok");
				close(fd); return 0;
			}
			if (errno != 0 && errno != ENETUNREACH && errno != EHOSTUNREACH &&
			    errno != EADDRNOTAVAIL && errno != ETIMEDOUT)
				return 1;
			usleep(100000);
		}
		return 1;
	}
	if (!strcmp(argv[1], "keep")) {
		struct pollfd p = { .fd = fd, .events = POLLIN | POLLOUT };
		marker("/run/netstack3-test-keep-ready");
		wait_for("/run/netstack3-test-check-link-down");
		sleep(1); poll(&p, 1, 0);
		if (p.revents & (POLLERR | POLLHUP)) return 1;
		marker("/run/netstack3-test-kept-on-link-down");
		close(fd); return 0;
	}
	if (send(fd, message, strlen(message), 0) != (ssize_t)strlen(message)) {
		perror("send"); return 1;
	}
	n = recv_with_timeout(fd, reply, sizeof(reply), 5000);
	if (n != (int)strlen(message) || memcmp(reply, message, n)) {
		if (n < 0) perror("recv");
		else fprintf(stderr, "unexpected echo payload\n");
		return 1;
	}
	if (!strcmp(argv[1], "survive")) {
		marker("/run/netstack3-test-survive-ready");
		wait_for("/run/netstack3-test-check-loss");
		for (;;) {
			ssize_t sent;
			errno = 0;
			sent = send(fd, message, strlen(message), 0);
			if (sent < 0) {
				if (errno == ENETUNREACH || errno == EHOSTUNREACH || errno == EADDRNOTAVAIL)
					break;
				perror("unexpected lease-loss send error"); return 1;
			}
			if (sent == (ssize_t)strlen(message))
				recv_with_timeout(fd, reply, sizeof(reply), 200);
			usleep(100000);
		}
		marker("/run/netstack3-test-survived-loss");
		wait_for("/run/netstack3-test-check-reacquired");
		for (int i = 0; i < 100; i++) {
			if (send(fd, message, strlen(message), 0) == (ssize_t)strlen(message) &&
			    (n = recv_with_timeout(fd, reply, sizeof(reply), 200)) == (int)strlen(message) &&
			    !memcmp(reply, message, n)) {
				marker("/run/netstack3-test-survived-reacquire");
				close(fd); return 0;
			}
			usleep(100000);
		}
		return 1;
	}
	close(fd);
	return 0;
}
