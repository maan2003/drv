// SPDX-License-Identifier: GPL-2.0-only
#define _GNU_SOURCE
#include <sys/socket.h>
#include <sys/ioctl.h>
#include <sys/epoll.h>
#include <sys/wait.h>
#include <sched.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include "protocol.h"

static void check(int ok, const char *what)
{
	if (!ok) { fprintf(stderr, "FAIL %s errno=%d (%s)\n", what, errno, strerror(errno)); exit(1); }
}
static int provider(void)
{
	int fd = open("/dev/ns3-rust-lifecycle", O_RDWR | O_CLOEXEC);
	check(fd >= 0, "provider open"); return fd;
}
static int app(int family)
{
	int fd = socket(family, SOCK_STREAM | SOCK_CLOEXEC, 0);
	check(fd >= 0, "socket create"); return fd;
}
static unsigned long long id(int fd)
{
	__u64 id;
	check(ioctl(fd, NSRL_ID, &id) == 0, "socket identity"); return id;
}
static void ready(int provider, int fd, int value)
{
	struct nsrl_ready update = { .id = id(fd), .ready = value };
	check(ioctl(provider, NSRL_READY, &update) == 0, "provider ready");
}
static struct nsrl_stats stats(int provider)
{
	struct nsrl_stats stats;
	check(ioctl(provider, NSRL_STATS, &stats) == 0, "Rust owner counters");
	return stats;
}
static int epoll(int fd)
{
	int ep = epoll_create1(EPOLL_CLOEXEC);
	struct epoll_event event = { .events = EPOLLIN | EPOLLOUT, .data.fd = fd };
	check(ep >= 0 && epoll_ctl(ep, EPOLL_CTL_ADD, fd, &event) == 0, "epoll setup");
	return ep;
}
static void events(int ep, unsigned int expected)
{
	struct epoll_event event;
	int n = epoll_wait(ep, &event, 1, expected ? 3000 : 0);
	check(n == (expected ? 1 : 0), "epoll event count");
	if (expected) check(event.events == expected, "epoll event mask");
}
static void done(pid_t child)
{
	int status;
	check(waitpid(child, &status, 0) == child && WIFEXITED(status) &&
	      WEXITSTATUS(status) == 0, "child exit status");
}
static void byte(int fd, int sending)
{
	char c = 'x';
	check((sending ? write(fd, &c, 1) : read(fd, &c, 1)) == 1, "pipe synchronization");
}
static void family(int family)
{
	int p = provider();
	check(open("/dev/ns3-rust-lifecycle", O_RDWR) < 0 && errno == EBUSY, "one provider per namespace");
	int fd = app(family), ep = epoll(fd);
	check(stats(p).sockets == 1, "one Rust socket owner");
	events(ep, 0);
	ready(p, fd, 1); events(ep, EPOLLIN | EPOLLOUT);
	ready(p, fd, 0); events(ep, 0);

	/* App dup/fork: neither descriptor close nor parent exit branch removes
	 * the Rust socket before the last VFS reference has gone. */
	int gate[2]; check(pipe(gate) == 0, "app fork gate");
	int copy = dup(fd); check(copy >= 0, "app dup");
	unsigned long long socket_id = id(fd);
	pid_t child = fork(); check(child >= 0, "app fork");
	if (!child) {
		close(gate[1]); close(p); close(fd); close(ep);
		byte(gate[0], 0); close(copy); close(gate[0]); _exit(0);
	}
	close(gate[0]); close(fd); close(copy); close(ep);
	check(stats(p).sockets == 1, "fork retains Rust socket");
	struct nsrl_ready update = { .id = socket_id, .ready = 1 };
	check(ioctl(p, NSRL_READY, &update) == 0, "fork owner still reachable");
	byte(gate[1], 1); close(gate[1]); done(child);
	check(stats(p).sockets == 0, "final app file release destroys Rust owner");
	check(ioctl(p, NSRL_READY, &update) < 0 && errno == ENOENT, "closed identity cannot be updated");

	/* Provider dup/fork: final process death, not closing one descriptor,
	 * revokes the generation and wakes existing app pollers. */
	fd = app(family); ep = epoll(fd);
	int pc = dup(p); check(pc >= 0 && pipe(gate) == 0, "provider dup gate");
	child = fork(); check(child >= 0, "provider fork");
	if (!child) {
		close(gate[1]); close(p); close(fd); close(ep);
		byte(gate[0], 0);
		/* Deliberately let exit release pc, exercising provider process death. */
		_exit(0);
	}
	close(gate[0]); close(p); close(pc);
	check(open("/dev/ns3-rust-lifecycle", O_RDWR) < 0 && errno == EBUSY, "fork retains provider generation");
	events(ep, 0);
	byte(gate[1], 1); close(gate[1]);
	events(ep, EPOLLERR | EPOLLHUP); done(child);
	check(socket(family, SOCK_STREAM, 0) < 0 && errno == ENETDOWN, "provider absent fails closed");
	p = provider();
	events(ep, EPOLLERR | EPOLLHUP);
	update = (struct nsrl_ready){ .id = id(fd), .ready = 1 };
	check(ioctl(p, NSRL_READY, &update) < 0 && errno == ENOENT, "replacement rejects old-generation identity");
	int fresh = app(family), fresh_ep = epoll(fresh);
	events(fresh_ep, 0); ready(p, fresh, 1); events(fresh_ep, EPOLLIN | EPOLLOUT);
	close(fresh_ep); close(fresh); close(ep); close(fd);
	check(stats(p).sockets == 0, "all socket owners released");
	close(p);
	printf("PASS RUST_LIFECYCLE_V%d_CREATE_POLL_DUP_FORK_DEATH_REPLACE\n", family == AF_INET ? 4 : 6);
}
static volatile sig_atomic_t interrupted;
static void signal_handler(int signal) { (void)signal; interrupted = 1; }
static void cancellation(void)
{
	int p = provider(), fd = app(AF_INET), ep = epoll(fd), gate[2];
	check(pipe(gate) == 0, "cancel gate");
	pid_t child = fork(); check(child >= 0, "cancel fork");
	if (!child) {
		close(gate[0]); close(p);
		struct sigaction action = { .sa_handler = signal_handler };
		check(sigaction(SIGUSR1, &action, NULL) == 0, "cancel sigaction");
		sigset_t blocked, previous;
		sigemptyset(&blocked); sigaddset(&blocked, SIGUSR1);
		check(sigprocmask(SIG_BLOCK, &blocked, &previous) == 0, "cancel block signal");
		byte(gate[1], 1);
		struct epoll_event event;
		check(epoll_pwait(ep, &event, 1, 3000, &previous) < 0 && errno == EINTR &&
		      interrupted, "interrupt blocking poll atomically");
		close(ep); close(fd); close(gate[1]); _exit(0);
	}
	close(gate[1]); byte(gate[0], 0);
	check(kill(child, SIGUSR1) == 0, "cancel signal"); done(child); close(gate[0]);
	check(stats(p).sockets == 1, "poll cancellation retains parent socket");
	ready(p, fd, 1); events(ep, EPOLLIN | EPOLLOUT);
	close(ep); close(fd); check(stats(p).sockets == 0, "cancel owner cleanup"); close(p);
	puts("PASS RUST_POLL_CANCELLATION");
}
static void quota_and_validation(void)
{
	int p = provider(), held[256];
	for (int i = 0; i < 256; ++i) held[i] = app(i % 2 ? AF_INET6 : AF_INET);
	check(socket(AF_INET, SOCK_STREAM, 0) < 0 && errno == ENFILE, "Rust quota bound");
	int copy = dup(held[0]); check(copy >= 0, "quota dup");
	close(held[0]);
	check(socket(AF_INET, SOCK_STREAM, 0) < 0 && errno == ENFILE, "dup retains quota");
	close(copy); held[0] = app(AF_INET);
	struct nsrl_ready update = { .id = id(held[0]), .ready = 2 };
	check(ioctl(p, NSRL_READY, &update) < 0 && errno == EINVAL, "invalid readiness rejected");
	update.ready = 1; update.reserved = 1;
	check(ioctl(p, NSRL_READY, &update) < 0 && errno == EINVAL, "reserved field rejected");
	check(ioctl(p, NSRL_READY, (void *)1) < 0 && errno == EFAULT, "bad user pointer rejected");
	for (int i = 0; i < 256; ++i) close(held[i]);
	check(stats(p).sockets == 0, "quota cleanup");
	close(p);
	puts("PASS RUST_QUOTA_VALIDATION");
}
static void namespace_isolation(void)
{
	int p = provider(), fd = app(AF_INET), ep = epoll(fd);
	struct nsrl_stats before = stats(p);
	pid_t child = fork(); check(child >= 0, "namespace fork");
	if (!child) {
		close(p); close(fd); close(ep);
		check(unshare(CLONE_NEWNET) == 0, "new network namespace");
		check(socket(AF_INET, SOCK_STREAM, 0) < 0 && errno == ENETDOWN, "no inherited namespace provider");
		int other = provider(), socket = app(AF_INET6);
		ready(other, socket, 1);
		close(socket); close(other); _exit(0);
	}
	done(child); events(ep, 0);
	/* Namespace cleanup is asynchronous; wait only for diagnostic reclamation. */
	for (int i = 0; i < 1000 && stats(p).namespaces != before.namespaces; ++i) usleep(1000);
	check(stats(p).namespaces == before.namespaces, "Rust namespace owner reclaimed");
	check(stats(p).sockets == before.sockets, "isolated namespace sockets reclaimed");
	close(ep); close(fd); close(p);
	puts("PASS RUST_NETNS_ISOLATION_RECLAMATION");
}
static void repeated_wakes(void)
{
	/* Vary scheduling around registration/sampling vs readiness/death updates.
	 * Pipe ordering provides synchronization; no sleep-based success oracle. */
	int p = provider();
	for (int i = 0; i < 100; ++i) {
		int fd = app(i % 2 ? AF_INET6 : AF_INET), ep = epoll(fd), gate[2];
		check(pipe(gate) == 0, "wake gate");
		pid_t child = fork(); check(child >= 0, "wake fork");
		if (!child) {
			close(gate[1]); byte(gate[0], 0);
			ready(p, fd, 1); _exit(0);
		}
		close(gate[0]); byte(gate[1], 1);
		events(ep, EPOLLIN | EPOLLOUT); done(child);
		ready(p, fd, 0); events(ep, 0);
		close(gate[1]); close(ep); close(fd);
	}
	check(stats(p).sockets == 0, "repeated wake ownership"); close(p);
	puts("PASS RUST_WAKE_RACES_100");
}
static void provider_sigkill(void)
{
	int online[2], gate[2];
	check(pipe(online) == 0 && pipe(gate) == 0, "crash pipes");
	pid_t child = fork(); check(child >= 0, "crash fork");
	if (!child) {
		close(online[0]); close(gate[1]);
		int p = provider(); (void)p;
		byte(online[1], 1);
		byte(gate[0], 0); /* Parent kills us without releasing this wait. */
		_exit(1);
	}
	close(online[1]); close(gate[0]); byte(online[0], 0); close(online[0]);
	int fd = app(AF_INET), ep = epoll(fd);
	events(ep, 0);
	check(kill(child, SIGKILL) == 0, "provider SIGKILL");
	events(ep, EPOLLERR | EPOLLHUP);
	int status;
	check(waitpid(child, &status, 0) == child && WIFSIGNALED(status) &&
	      WTERMSIG(status) == SIGKILL, "provider killed");
	close(gate[1]);
	int replacement = provider();
	events(ep, EPOLLERR | EPOLLHUP);
	close(ep); close(fd);
	check(stats(replacement).sockets == 0, "crash owner recovery"); close(replacement);
	puts("PASS RUST_PROVIDER_SIGKILL");
}
static void concurrent_lifetimes(void)
{
	enum { WORKERS = 8, ITERATIONS = 500 };
	int p = provider();
	pid_t children[WORKERS];
	for (int i = 0; i < WORKERS; ++i) {
		children[i] = fork(); check(children[i] >= 0, "concurrent fork");
		if (!children[i]) {
			for (int j = 0; j < ITERATIONS; ++j) {
				int fd = app((i + j) % 2 ? AF_INET6 : AF_INET), ep = epoll(fd);
				int copy = dup(fd); check(copy >= 0, "concurrent dup");
				close(fd);
				events(ep, 0);
				ready(p, copy, 1); events(ep, EPOLLIN | EPOLLOUT);
				ready(p, copy, 0); events(ep, 0);
				close(ep); close(copy);
			}
			close(p); _exit(0);
		}
	}
	for (int i = 0; i < WORKERS; ++i) done(children[i]);
	check(stats(p).sockets == 0, "concurrent owner counters return to zero"); close(p);
	puts("PASS RUST_CONCURRENT_LIFETIMES_8x500");
}
int main(void)
{
	setbuf(stdout, NULL); alarm(40);
	check(socket(AF_INET, SOCK_STREAM, 0) < 0 && errno == ENETDOWN, "no native IPv4 fallback");
	check(socket(AF_INET6, SOCK_STREAM, 0) < 0 && errno == ENETDOWN, "no native IPv6 fallback");
	family(AF_INET); family(AF_INET6);
	cancellation(); quota_and_validation(); namespace_isolation(); repeated_wakes();
	provider_sigkill(); concurrent_lifetimes();
	puts("PASS RUST_LIFECYCLE_SUITE");
	return 0;
}
