// SPDX-License-Identifier: GPL-2.0-only
#define _GNU_SOURCE
#include <sys/socket.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <arpa/inet.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <time.h>
#include "protocol.h"
static void check(int ok, const char *what) {
	if (!ok) { fprintf(stderr, "FAIL endpoint %s errno=%d\n", what, errno); exit(1); }
}
static int claim(int registration, unsigned long long *id) {
	int fd = ioctl(registration, NS3_CLAIM, id);
	check(fd >= 0, "claim");
	check(fcntl(fd, F_GETFD) & FD_CLOEXEC, "claim CLOEXEC");
	return fd;
}
static void receive_packet(int endpoint, unsigned long long id) {
	struct { struct ns3_msg h; struct ns3_addr a; char data[4]; } frame = {
		.h = {.version=NS3_VERSION, .op=NS3_RX, .socket=id, .len=28},
		.a = {.family=4, .port=23460}, .data={'t','e','s','t'},
	};
	check(write(endpoint, &frame, sizeof(frame.h) + 28) == sizeof(frame.h) + 28, "inject bounded packet");
}
static void interrupted(int sig) { (void)sig; }
int main(int argc, char **argv) {
	alarm(20); setbuf(stdout, NULL);
	int registration = open("/dev/netstack3", O_RDWR | O_CLOEXEC);
	check(registration >= 0, "register");
    if (argc == 2 && !strcmp(argv[1], "bench")) {
        int app = socket(AF_INET, SOCK_DGRAM | SOCK_NONBLOCK, 0);
        unsigned long long id;
        int endpoint = claim(registration, &id);
        char packet[sizeof(struct ns3_msg) + sizeof(struct ns3_addr) + NS3_PAYLOAD] = {0};
        struct ns3_msg *h = (void *)packet;
        h->version = NS3_VERSION; h->op = NS3_RX; h->socket = id;
        h->len = sizeof(struct ns3_addr) + NS3_PAYLOAD;
        struct ns3_addr *address = (void *)(h + 1); address->family = 4;
        char data[NS3_PAYLOAD], credit[64];
        struct timespec begin, end, cpu_begin, cpu_end;
        clock_gettime(CLOCK_MONOTONIC, &begin);
        clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &cpu_begin);
        for (int i=0; i<100000; i++) {
            check(write(endpoint, packet, sizeof(packet)) == sizeof(packet), "benchmark write");
            check(recv(app, data, sizeof(data), 0) == sizeof(data), "benchmark receive");
            check(read(endpoint, credit, sizeof(credit)) == sizeof(struct ns3_msg) + 4, "benchmark credit");
        }
        clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &cpu_end);
        clock_gettime(CLOCK_MONOTONIC, &end);
        double seconds = end.tv_sec-begin.tv_sec+(end.tv_nsec-begin.tv_nsec)/1e9;
        double cpu = cpu_end.tv_sec-cpu_begin.tv_sec+(cpu_end.tv_nsec-cpu_begin.tv_nsec)/1e9;
        printf("BENCH IPC frames=100000 bytes=1638400000 seconds=%.6f cpu_seconds=%.6f MBps=%.2f\n", seconds, cpu, 1638.4/seconds);
        close(app); close(endpoint); close(registration);
        return 0;
    }
	int a = socket(AF_INET, SOCK_DGRAM | SOCK_NONBLOCK, 0);
	int b = socket(AF_INET, SOCK_DGRAM | SOCK_NONBLOCK, 0);
	check(a >= 0 && b >= 0, "application sockets");
	unsigned long long aid, bid;
	int ap = claim(registration, &aid), bp = claim(registration, &bid);
	struct { struct ns3_msg h; unsigned int state; } forged = {
        .h = {.version=NS3_VERSION, .op=NS3_STATE, .socket=bid, .len=4}, .state=NS3_CONNECTED};
	check(write(ap, &forged, sizeof(forged.h) + 4) < 0 && errno == EPROTO, "cross-FD identity rejected");
	struct sockaddr_in dest = {.sin_family=AF_INET,.sin_port=htons(23460),.sin_addr.s_addr=htonl(0x7f000001)};
	char data[NS3_PAYLOAD] = {0};
	size_t admitted = 0;
	for (;;) {
		ssize_t n = sendto(a, data, sizeof(data), 0, (void *)&dest, sizeof(dest));
		if (n < 0) { check(errno == EAGAIN, "bounded admission"); break; }
		admitted += n;
		check(admitted <= 256 * 1024, "bounded socket bytes");
	}
	check(admitted > 0, "admitted data");
	receive_packet(bp, bid);
	check(recv(b, data, 4, 0) == 4 && !memcmp(data, "test", 4), "other socket progresses under saturation");
	close(a);
	int saw_close = 0;
	char frame[sizeof(struct ns3_msg) + sizeof(struct ns3_addr) + NS3_PAYLOAD];
	for (int i=0; i<64; i++) {
		ssize_t n = read(ap, frame, sizeof(frame));
		if (n < 0) { check(errno == EAGAIN, "endpoint drain"); break; }
		if (((struct ns3_msg *)frame)->op == NS3_CLOSE) saw_close = 1;
	}
	check(saw_close, "close notification survives saturated unacknowledged data");
	close(ap);
	receive_packet(bp, bid);
	check(recv(b, data, 4, 0) == 4, "endpoint close does not revoke another socket");
	puts("PASS ENDPOINT_SCOPING_SATURATION_CLOSE");
	/* Interrupt a real control wait while the provider withholds completion. */
	int c = socket(AF_INET, SOCK_DGRAM, 0);
	unsigned long long cid;
	int cp = claim(registration, &cid);
	pid_t child = fork();
	check(child >= 0, "fork");
	if (!child) {
		struct sigaction action = {.sa_handler=interrupted};
		sigaction(SIGUSR1, &action, NULL);
		int r = bind(c, (void *)&dest, sizeof(dest));
		_exit(r < 0 && errno == EINTR ? 0 : 1);
	}
	int saw_bind = 0;
	for (int i=0; i<1000 && !saw_bind; i++) {
		ssize_t n = read(cp, frame, sizeof(frame));
		if (n > 0 && ((struct ns3_msg *)frame)->op == NS3_BIND) saw_bind=1;
		else usleep(1000);
	}
	check(saw_bind, "control request reached provider");
	kill(child, SIGUSR1);
	int status;
	check(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status)==0, "control wait interrupted");
	receive_packet(bp, bid);
	check(recv(b, data, 4, 0) == 4, "interruption leaves other endpoint alive");
	/* Closing one provider endpoint revokes its application socket. */
	close(bp);
	check(sendto(b, "x", 1, 0, (void *)&dest, sizeof(dest)) < 0 && errno == ENETDOWN, "provider endpoint death");
	int d = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
	check(d >= 0, "registration survives individual death");
	check(connect(d, (void *)&dest, sizeof(dest)) < 0 && errno == EINPROGRESS, "connect does not await provider claim or OPEN");
	puts("PASS ENDPOINT_CANCEL_AND_NONBLOCK");
	close(c); close(cp); close(b); close(d);
    int held[300], count=0;
    for (; count<300; count++) {
        int app = socket(AF_INET, SOCK_DGRAM, 0);
        if (app < 0) { check(errno == ENFILE, "retained endpoint quota"); break; }
        unsigned long long id;
        held[count] = claim(registration, &id);
        close(app);
    }
    check(count == 256, "closed application still charged while provider retains endpoint");
    for (int i=0; i<count; i++) close(held[i]);
    int recovered = socket(AF_INET, SOCK_DGRAM, 0);
    check(recovered >= 0, "quota released by final endpoint lifetime");
    close(recovered);
    puts("PASS ENDPOINT_LIFETIME_QUOTA");
    close(registration);
	return 0;
}
