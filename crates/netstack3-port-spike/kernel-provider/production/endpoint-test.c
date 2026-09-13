// SPDX-License-Identifier: GPL-2.0-only
#define _GNU_SOURCE
#include <sys/socket.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <sys/resource.h>
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
    /* A fault copying the claim ID must not consume a queued socket. */
    int fault_app = socket(AF_INET, SOCK_DGRAM | SOCK_NONBLOCK, 0);
    check(fault_app >= 0, "claim fault socket");
    check(ioctl(registration, NS3_CLAIM, (void *)1) < 0 && errno == EFAULT,
          "claim user-copy rollback");
    struct rlimit original, limited;
    check(getrlimit(RLIMIT_NOFILE, &original) == 0, "read FD limit");
    limited = original; limited.rlim_cur = 64;
    check(setrlimit(RLIMIT_NOFILE, &limited) == 0, "lower FD limit");
    int fillers[64], filled = 0;
    while (filled < 64) {
        int fd = dup(registration);
        if (fd < 0) { check(errno == EMFILE, "fill FD table"); break; }
        fillers[filled++] = fd;
    }
    unsigned long long unclaimed_id = 0;
    check(ioctl(registration, NS3_CLAIM, &unclaimed_id) < 0 && errno == EMFILE,
          "claim FD reservation rollback");
    while (filled) close(fillers[--filled]);
    check(setrlimit(RLIMIT_NOFILE, &original) == 0, "restore FD limit");
    unsigned long long fault_id;
    int fault_endpoint = claim(registration, &fault_id);
    receive_packet(fault_endpoint, fault_id);
    char fault_byte[4];
    check(recv(fault_app, fault_byte, sizeof(fault_byte), 0) == 4,
          "claim succeeds after failed copy");
    close(fault_app); close(fault_endpoint);
    puts("PASS ENDPOINT_CLAIM_COPY_ROLLBACK");
	int a = socket(AF_INET, SOCK_DGRAM | SOCK_NONBLOCK, 0);
	int b = socket(AF_INET, SOCK_DGRAM | SOCK_NONBLOCK, 0);
	check(a >= 0 && b >= 0, "application sockets");
	unsigned long long aid, bid;
	int ap = claim(registration, &aid), bp = claim(registration, &bid);
	struct { struct ns3_msg h; unsigned int state; } forged = {
        .h = {.version=NS3_VERSION, .op=NS3_STATE, .socket=bid, .len=4}, .state=NS3_CONNECTED};
	check(write(ap, &forged, sizeof(forged.h) + 4) < 0 && errno == EPROTO, "cross-FD identity rejected");
    forged.h.socket = aid;
    forged.h.version = 4;
    check(write(ap, &forged, sizeof(forged.h) + 4) < 0 && errno == EPROTO,
          "ABI4 worker rejected before state mutation");
    puts("PASS ENDPOINT_ABI5_REJECTS_ABI4");
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
    /* RX admission is exactly four credits, reclaimed only when the provider
     * reads CREDIT, not merely when the application consumes data. */
    int q = socket(AF_INET, SOCK_DGRAM | SOCK_NONBLOCK, 0);
    unsigned long long qid;
    int qp = claim(registration, &qid);
    for (int i = 0; i < 4; ++i) receive_packet(qp, qid);
    struct { struct ns3_msg h; struct ns3_addr a; char bytes[4]; } packet = {
        .h = {.version=NS3_VERSION,.op=NS3_RX,.socket=qid,.len=28},
        .a = {.family=4}, .bytes = {'t','e','s','t'}
    };
    check(write(qp, &packet, sizeof(packet.h) + 28) < 0 && errno == EAGAIN,
          "fifth receive credit denied");
    for (int i = 0; i < 4; ++i) check(recv(q, data, 4, 0) == 4, "consume credited packet");
    check(write(qp, &packet, sizeof(packet.h) + 28) < 0 && errno == EAGAIN,
          "consumption alone cannot create provider credits");
    check(read(qp, frame, sizeof(frame)) == sizeof(struct ns3_msg) + 4 &&
          ((struct ns3_msg *)frame)->op == NS3_CREDIT, "credit batch returned");
    unsigned int credits;
    memcpy(&credits, frame + sizeof(struct ns3_msg), 4);
    check(credits == 4, "credit conservation");
    receive_packet(qp, qid);
    check(recv(q, data, 4, 0) == 4, "credit reuse");
    struct ns3_msg unknown = {.version=NS3_VERSION,.op=NS3_SEND,.socket=qid,.request=999};
    check(write(qp, &unknown, sizeof(unknown)) < 0 && errno == EPROTO,
          "unknown completion revokes endpoint");
    check(recv(q, data, 1, 0) < 0 && errno == ENETDOWN, "revoked endpoint fails closed");
    close(qp); close(q);
    puts("PASS ENDPOINT_CREDITS_AND_UNKNOWN_COMPLETION");

	/* Interrupt a real control wait while the provider withholds completion. */
    /* Read-only control cancellation must not revoke or steal a later result. */
    int query = socket(AF_INET, SOCK_DGRAM, 0);
    unsigned long long query_id;
    int query_ep = claim(registration, &query_id);
    for (int pass = 0; pass < 2; pass++) {
        pid_t reader = fork();
        check(reader >= 0, "query fork");
        if (!reader) {
            struct sigaction action = {.sa_handler=interrupted};
            sigaction(SIGUSR1, &action, NULL);
            struct sockaddr_storage name;
            socklen_t size = sizeof(name);
            int r = getsockname(query, (void *)&name, &size);
            _exit(pass == 0 ? !(r < 0 && errno == EINTR) :
                  !(r == 0 && name.ss_family == AF_INET &&
                    ((struct sockaddr_in *)&name)->sin_port == htons(23461)));
        }
        int saw_query = 0;
        for (int i = 0; i < 1000 && !saw_query; i++) {
            ssize_t n = read(query_ep, frame, sizeof(frame));
            if (n > 0) {
                struct ns3_msg *h = (void *)frame;
                if (h->op == NS3_GETNAME) saw_query = 1;
                else { h->len = 0; check(write(query_ep, h, sizeof(*h)) == sizeof(*h), "query OPEN"); }
            } else usleep(1000);
        }
        check(saw_query, "query reached provider");
        static struct ns3_msg old_query;
        int status;
        if (!pass) {
            old_query = *(struct ns3_msg *)frame;
            kill(reader, SIGUSR1);
            check(waitpid(reader, &status, 0) == reader && WIFEXITED(status) &&
                  WEXITSTATUS(status) == 0, "query interrupted");
        } else {
            /* An old error must neither complete this waiter nor set SO_ERROR. */
            old_query.len = 0; old_query.status = EINVAL;
            check(write(query_ep, &old_query, sizeof(old_query)) == sizeof(old_query),
                  "late query reply drained");
            struct ns3_msg *h = (void *)frame;
            h->len = sizeof(struct ns3_addr);
            struct ns3_addr answer = {.family=4, .port=23461};
            memcpy(h + 1, &answer, sizeof(answer));
            check(write(query_ep, frame, sizeof(*h) + sizeof(answer)) ==
                  sizeof(*h) + sizeof(answer), "new query reply");
            check(waitpid(reader, &status, 0) == reader && WIFEXITED(status) &&
                  WEXITSTATUS(status) == 0, "query result not stolen");
            int error = -1; socklen_t size = sizeof(error);
            check(getsockopt(query, SOL_SOCKET, SO_ERROR, &error, &size) == 0 &&
                  !error, "cancelled query error not published");
        }
    }
    close(query); close(query_ep);
    puts("PASS ENDPOINT_QUERY_CANCEL_LATE_REPLY");
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
    struct ns3_msg late = *(struct ns3_msg *)frame;
    late.len = 0;
    check(write(cp, &late, sizeof(late)) < 0 && errno == ENETDOWN,
          "late reply cannot resurrect interrupted control");
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
