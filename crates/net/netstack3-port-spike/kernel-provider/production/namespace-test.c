// SPDX-License-Identifier: GPL-2.0-only
#define _GNU_SOURCE
#include <fcntl.h>
#include <arpa/inet.h>
#include <errno.h>
#include <net/if.h>
#include <signal.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include "namespace-protocol.h"
#include <linux/netlink.h>
#include <linux/bpf.h>
#include <sys/syscall.h>
#include <linux/rtnetlink.h>
#include <poll.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { perror(#x); exit(1); } } while (0)

/* Service FDs must not keep a namespace alive. Application sockets and namespace
 * handles must. Exercise both protocol registrations, not just the INET path. */
static void lifetime(void)
{
    int original = open("/proc/self/ns/net", O_RDONLY | O_CLOEXEC);
    CHECK(original >= 0);
    for (int family = 0; family < 2; family++) {
        CHECK(unshare(CLONE_NEWNET) == 0);
        int ns = open("/proc/self/ns/net", O_RDONLY | O_CLOEXEC);
        int inet = open("/dev/netstack3", O_RDWR | O_CLOEXEC);
        int route = dup(inet);
        CHECK(ioctl(inet, NS3_NAMESPACE_READY) == 0);
        CHECK(ns >= 0 && inet >= 0 && route >= 0);
        int client = family ? socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_ROUTE)
                            : socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
        CHECK(client >= 0);
        CHECK(setns(original, CLONE_NEWNET) == 0);
        CHECK(close(ns) == 0);
        struct pollfd fds[2] = {{inet, 0, 0}, {route, 0, 0}};
        CHECK(poll(fds, 2, 50) == 0); /* client still owns the namespace */
        CHECK(close(client) == 0);
        for (int i = 0; i < 2; i++) {
            CHECK(poll(&fds[i], 1, 3000) == 1);
            CHECK(fds[i].revents & POLLHUP);
        }
        CHECK(close(inet) == 0 && close(route) == 0);
    }
    CHECK(close(original) == 0);
    puts("PASS_NAMESPACE_SERVICE_LIFETIME");
}


static int children(pid_t manager, pid_t *pids)
{
    char path[96], data[1024];
    snprintf(path, sizeof(path), "/proc/%d/task/%d/children", manager, manager);
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    CHECK(fd >= 0);
    ssize_t n = read(fd, data, sizeof(data) - 1);
    CHECK(n >= 0 && n < (ssize_t)sizeof(data) - 1);
    data[n] = 0;
    CHECK(close(fd) == 0);
    int count = 0, consumed;
    char *p = data;
    while (sscanf(p, "%d%n", &pids[count], &consumed) == 1) {
        CHECK(++count <= 64);
        p += consumed;
    }
    return count;
}
static void wait_children(pid_t manager, int expected)
{
    pid_t pids[65];
    for (int i = 0; i < 300; i++) {
        if (children(manager, pids) == expected) return;
        usleep(10000);
    }
    errno = ETIMEDOUT;
    CHECK(!"namespace worker count");
}
static pid_t start_manager(void)
{
    pid_t pid = fork();
    CHECK(pid >= 0);
    if (!pid) {
        execl("/bin/netstack3-supervisor", "netstack3-supervisor",
              "/bin/netstack3-provider", NULL);
        _exit(127);
    }
    /* Device ownership, not an arbitrary delay, proves manager registration. */
    for (int i = 0; i < 300; i++) {
        char path[96], target[128];
        snprintf(path, sizeof(path), "/proc/%d/fd/3", pid);
        ssize_t n = readlink(path, target, sizeof(target) - 1);
        if (n > 0) {
            target[n] = 0;
            if (!strcmp(target, "/dev/netstack3-namespaces")) return pid;
        }
        CHECK(kill(pid, 0) == 0);
        usleep(10000);
    }
    CHECK(!"namespace manager startup");
    return -1;
}
static int udp(unsigned short port)
{
    int fd = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    CHECK(fd >= 0);
    struct sockaddr_in addr = {.sin_family = AF_INET, .sin_port = htons(port),
        .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
    CHECK(bind(fd, (void *)&addr, sizeof(addr)) == 0);
    return fd;
}
static void loopback(int fd, int up)
{
    struct ifreq req = {};
    strcpy(req.ifr_name, "lo");
    req.ifr_flags = up ? IFF_UP : 0;
    CHECK(ioctl(fd, SIOCSIFFLAGS, &req) == 0);
}
static void interface_controls(void)
{
    CHECK(access("/sys/class/net/lo", F_OK) == -1 && errno == ENOENT);
    int fd = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    CHECK(fd >= 0);
    struct ifreq req = {};
    strcpy(req.ifr_name, "lo");
    CHECK(ioctl(fd, SIOCGIFINDEX, &req) == 0 && req.ifr_ifindex == 1);
    CHECK(ioctl(fd, SIOCGIFMTU, &req) == 0 && req.ifr_mtu == 65536);
    CHECK(ioctl(fd, SIOCGIFFLAGS, &req) == 0 && (req.ifr_flags & IFF_UP));
    struct ifconf conf = {};
    CHECK(ioctl(fd, SIOCGIFCONF, &conf) == 0 && conf.ifc_len == sizeof(req));
    struct ifreq address = {};
    conf.ifc_buf = (void *)&address;
    CHECK(ioctl(fd, SIOCGIFCONF, &conf) == 0 && conf.ifc_len == sizeof(address));
    CHECK(!strcmp(address.ifr_name, "lo"));
    CHECK(((struct sockaddr_in *)&address.ifr_addr)->sin_addr.s_addr == htonl(INADDR_LOOPBACK));
    conf.ifc_len = 1;
    CHECK(ioctl(fd, SIOCGIFCONF, &conf) == 0 && conf.ifc_len == 0);
    struct ifreq *readonly = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                                 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(readonly != MAP_FAILED);
    strcpy(readonly->ifr_name, "lo");
    readonly->ifr_flags = 0;
    CHECK(mprotect(readonly, 4096, PROT_READ) == 0);
    CHECK(ioctl(fd, SIOCSIFFLAGS, readonly) == 0);
    CHECK(ioctl(fd, SIOCGIFFLAGS, &req) == 0 && !(req.ifr_flags & IFF_UP));
    CHECK(munmap(readonly, 4096) == 0);
    loopback(fd, 1);
    CHECK(close(fd) == 0);
    puts("PASS_USERSPACE_INTERFACE_IOCTL_NO_NATIVE_LOOPBACK");
}

static void send_to(int fd, unsigned short port, char value)
{
    struct sockaddr_in addr = {.sin_family = AF_INET, .sin_port = htons(port),
        .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
    CHECK(sendto(fd, &value, 1, 0, (void *)&addr, sizeof(addr)) == 1);
}
static void receive(int fd, char expected)
{
    struct pollfd p = {fd, POLLIN, 0};
    CHECK(poll(&p, 1, 3000) == 1);
    char value;
    CHECK(recv(fd, &value, 1, 0) == 1 && value == expected);
}
static void revoked(int fd)
{
    struct pollfd p = {fd, 0, 0};
    CHECK(poll(&p, 1, 3000) == 1);
    CHECK(p.revents & (POLLERR | POLLHUP));
}
static int transfer(int fd)
{
    int pair[2];
    CHECK(socketpair(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0, pair) == 0);
    char value = 0, control[CMSG_SPACE(sizeof(int))] = {};
    struct iovec iov = {&value, 1};
    struct msghdr msg = {.msg_iov = &iov, .msg_iovlen = 1,
        .msg_control = control, .msg_controllen = sizeof(control)};
    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
    cmsg->cmsg_level = SOL_SOCKET; cmsg->cmsg_type = SCM_RIGHTS;
    cmsg->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cmsg), &fd, sizeof(fd));
    CHECK(sendmsg(pair[0], &msg, 0) == 1);
    CHECK(recvmsg(pair[1], &msg, MSG_CMSG_CLOEXEC) == 1);
    int result;
    memcpy(&result, CMSG_DATA(CMSG_FIRSTHDR(&msg)), sizeof(result));
    CHECK(close(pair[0]) == 0 && close(pair[1]) == 0);
    return result;
}
static void netlink_loopback(int up, int expected_error)
{
    int nl = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_ROUTE);
    CHECK(nl >= 0);
    struct { struct nlmsghdr h; struct ifinfomsg i; } request = {
        .h = {.nlmsg_len = sizeof(request), .nlmsg_type = RTM_NEWLINK,
          .nlmsg_flags = NLM_F_REQUEST | NLM_F_ACK, .nlmsg_seq = 42,
          .nlmsg_pid = 1}, /* a forged privileged sender is not authority */
        .i = {.ifi_index = 1, .ifi_flags = up ? IFF_UP : 0, .ifi_change = IFF_UP}};
    struct sockaddr_nl kernel = {.nl_family = AF_NETLINK};
    CHECK(sendto(nl, &request, sizeof(request), 0, (void *)&kernel, sizeof(kernel)) == sizeof(request));
    struct pollfd ready = {nl, POLLIN, 0};
    CHECK(poll(&ready, 1, 3000) == 1);
    char response[256];
    CHECK(recv(nl, response, sizeof(response), 0) >= (ssize_t)(NLMSG_HDRLEN + sizeof(struct nlmsgerr)));
    struct nlmsghdr *header = (void *)response;
    struct nlmsgerr *error = NLMSG_DATA(header);
    CHECK(header->nlmsg_type == NLMSG_ERROR && error->error == -expected_error);
    CHECK(close(nl) == 0);
}
static void denied_admin(int fd)
{
    pid_t child = fork();
    CHECK(child >= 0);
    if (!child) {
        CHECK(setgid(65534) == 0 && setuid(65534) == 0);
        struct ifreq req = {};
        strcpy(req.ifr_name, "lo");
        CHECK(ioctl(fd, SIOCSIFFLAGS, &req) == -1 && errno == EPERM);
        netlink_loopback(0, EPERM);
        _exit(0);
    }
    int status;
    CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
}
static void byte(int fd, int write_byte)
{
    char value = 'x';
    CHECK((write_byte ? write(fd, &value, 1) : read(fd, &value, 1)) == 1);
}
static struct ns3_namespace_claim claim(int broker)
{
    struct pollfd ready = {broker, POLLIN, 0};
    CHECK(poll(&ready, 1, 3000) == 1);
    struct ns3_namespace_claim result;
    CHECK(ioctl(broker, NS3_NAMESPACE_CLAIM, &result) == 0);
    CHECK(ioctl(result.provider_fd, NS3_NAMESPACE_READY) == 0);
    return result;
}
static void broker_failures(void)
{
    int broker = open("/dev/netstack3-namespaces", O_RDWR | O_CLOEXEC);
    CHECK(broker >= 0);
    CHECK(open("/dev/netstack3-namespaces", O_RDWR | O_CLOEXEC) == -1 && errno == EBUSY);
    int request[2], response[2];
    CHECK(pipe2(request, O_CLOEXEC) == 0 && pipe2(response, O_CLOEXEC) == 0);
    pid_t child = fork();
    CHECK(child >= 0);
    if (!child) {
        CHECK(close(broker) == 0 && close(request[1]) == 0 && close(response[0]) == 0);
        CHECK(unshare(CLONE_NEWNET) == 0);
        for (int round = 0; round < 3; round++) {
            int fd = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
            if (!round) CHECK(fd == -1 && errno == ENETDOWN);
            else CHECK(fd >= 0);
            byte(response[1], 1);
            byte(request[0], 0);
            if (fd >= 0) { revoked(fd); CHECK(close(fd) == 0); }
        }
        _exit(0);
    }
    CHECK(close(request[0]) == 0 && close(response[1]) == 0);
    struct pollfd ready = {broker, POLLIN, 0};
    CHECK(poll(&ready, 1, 3000) == 1);
    /* Faulted publication must undo both registrations and wake the waiter. */
    CHECK(ioctl(broker, NS3_NAMESPACE_CLAIM, (void *)1) == -1 && errno == EFAULT);
    byte(response[0], 0); byte(request[1], 1);
    struct ns3_namespace_claim old = claim(broker);
    byte(response[0], 0);
    CHECK(ioctl(old.monitor_fd, NS3_NAMESPACE_REVOKE) == 0);
    revoked(old.provider_fd);
    byte(request[1], 1);
    struct ns3_namespace_claim next = claim(broker);
    byte(response[0], 0);
    CHECK(ioctl(old.provider_fd, NS3_NAMESPACE_READY) == -1 && errno == ENETDOWN);
    CHECK(ioctl(old.provider_fd, NS3_NAMESPACE_CONTROL) == -1 && errno == ENETDOWN);
    CHECK(ioctl(old.monitor_fd, NS3_NAMESPACE_REVOKE) == 0);
    CHECK(close(old.provider_fd) == 0 && close(old.monitor_fd) == 0);
    CHECK(ioctl(next.provider_fd, NS3_NAMESPACE_READY) == 0);
    CHECK(close(broker) == 0);
    revoked(next.provider_fd); revoked(next.monitor_fd);
    byte(request[1], 1);
    int status;
    CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    CHECK(close(next.provider_fd) == 0 && close(next.monitor_fd) == 0);
    CHECK(close(request[1]) == 0 && close(response[0]) == 0);
    puts("PASS_NAMESPACE_CLAIM_FAILURE_AND_GENERATION_FENCES");
}
static void managed(void)
{
    int original = open("/proc/self/ns/net", O_RDONLY | O_CLOEXEC);
    CHECK(original >= 0);
    pid_t manager = start_manager(), pids[65];
    wait_children(manager, 0);
    CHECK(unshare(CLONE_NEWNET) == 0);
    int a = open("/proc/self/ns/net", O_RDONLY | O_CLOEXEC);
    CHECK(a >= 0);
    wait_children(manager, 0); /* creating a namespace is not eager spawning */
    int control_a = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    CHECK(control_a >= 0);
    wait_children(manager, 1);
    CHECK(children(manager, pids) == 1);
    pid_t worker_a = pids[0];
    loopback(control_a, 1);
    interface_controls();
    int server_a = udp(32001), client_a = udp(32002);
    CHECK(setns(original, CLONE_NEWNET) == 0);
    CHECK(unshare(CLONE_NEWNET) == 0);
    int b = open("/proc/self/ns/net", O_RDONLY | O_CLOEXEC);
    CHECK(b >= 0);
    int control_b = socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    CHECK(control_b >= 0);
    loopback(control_b, 1);
    int server_b = udp(32001), client_b = udp(32002);
    denied_admin(control_b);
    struct ifreq unsupported = {};
    strcpy(unsupported.ifr_name, "lo");
    unsupported.ifr_flags = IFF_UP | IFF_PROMISC;
    CHECK(ioctl(control_b, SIOCSIFFLAGS, &unsupported) == -1 && errno == EOPNOTSUPP);
    puts("PASS_NAMESPACE_ADMIN_AUTHORITY");
    wait_children(manager, 2);
    int passed = transfer(client_a);
    CHECK(close(client_a) == 0);
    /* Both realms use identical local ports. FD authority follows A even in B. */
    send_to(passed, 32001, 'A');
    receive(server_a, 'A');
    struct pollfd empty = {server_b, POLLIN, 0};
    CHECK(poll(&empty, 1, 20) == 0);
    send_to(client_b, 32001, 'B');
    receive(server_b, 'B');
    netlink_loopback(0, 0);
    struct sockaddr_in addr = {.sin_family = AF_INET, .sin_port = htons(32001),
        .sin_addr.s_addr = htonl(INADDR_LOOPBACK)};
    /* TX admission is asynchronous; test delivery and the resulting socket error. */
    CHECK(sendto(client_b, "x", 1, 0, (void *)&addr, sizeof(addr)) == 1);
    struct pollfd failed = {client_b, 0, 0};
    CHECK(poll(&failed, 1, 3000) == 1 && (failed.revents & POLLERR));
    int error; socklen_t error_len = sizeof(error);
    CHECK(getsockopt(client_b, SOL_SOCKET, SO_ERROR, &error, &error_len) == 0);
    CHECK(error == ENETUNREACH || error == EHOSTUNREACH || error == ENETDOWN);
    empty.revents = 0;
    CHECK(poll(&empty, 1, 20) == 0);
    netlink_loopback(1, 0);
    send_to(client_b, 32001, 'b'); receive(server_b, 'b');
    CHECK(kill(worker_a, SIGKILL) == 0);
    revoked(server_a);
    send_to(client_b, 32001, 'B'); receive(server_b, 'B');
    CHECK(setns(a, CLONE_NEWNET) == 0);
    int replacement_control = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    CHECK(replacement_control >= 0);
    loopback(replacement_control, 1);
    CHECK(close(replacement_control) == 0);
    int replacement = udp(32001), replacement_client = udp(32002);
    send_to(replacement_client, 32001, 'R'); receive(replacement, 'R');
    CHECK(sendto(passed, "x", 1, 0, (void *)&addr, sizeof(addr)) == -1);
    CHECK(errno == ENETDOWN);
    wait_children(manager, 2);
    puts("PASS_NAMESPACE_ISOLATION_FD_TRANSFER_AND_WORKER_RESTART");
    CHECK(setns(original, CLONE_NEWNET) == 0);
    CHECK(kill(manager, SIGKILL) == 0);
    CHECK(waitpid(manager, NULL, 0) == manager);
    revoked(replacement); revoked(server_b);
    CHECK(setns(b, CLONE_NEWNET) == 0);
    CHECK(socket(AF_INET, SOCK_DGRAM | SOCK_CLOEXEC, 0) == -1 && errno == ENETDOWN);
    CHECK(setns(original, CLONE_NEWNET) == 0);
    manager = start_manager();
    CHECK(setns(b, CLONE_NEWNET) == 0);
    int next_control = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    CHECK(next_control >= 0);
    loopback(next_control, 1);
    CHECK(close(next_control) == 0);
    int next = udp(32001), next_client = udp(32002);
    send_to(next_client, 32001, 'M'); receive(next, 'M');
    puts("PASS_NAMESPACE_MANAGER_RESTART");
    CHECK(setns(original, CLONE_NEWNET) == 0);
    int fds[] = {a,b,control_a,control_b,server_a,passed,server_b,client_b,
                 replacement,replacement_client,next,next_client,original};
    for (unsigned i = 0; i < sizeof(fds)/sizeof(fds[0]); i++) CHECK(close(fds[i]) == 0);
    wait_children(manager, 0);
    CHECK(kill(manager, SIGTERM) == 0);
    CHECK(waitpid(manager, NULL, 0) == manager);
    puts("PASS_NAMESPACE_LAZY_PROVISION_AND_REAP");
}
static void native_boundaries(void)
{
    CHECK(access("/sys/class/net/lo", F_OK) == -1 && errno == ENOENT);
    int fd = socket(AF_PACKET, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    CHECK(fd >= 0 && close(fd) == 0);
    fd = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_GENERIC);
    CHECK(fd >= 0 && close(fd) == 0);
    const unsigned types[] = {BPF_PROG_TYPE_SOCKET_FILTER, BPF_PROG_TYPE_XDP};
    for (unsigned i = 0; i < sizeof(types)/sizeof(types[0]); i++) {
        struct bpf_insn code[] = {
            {.code = BPF_ALU64 | BPF_MOV | BPF_K, .dst_reg = BPF_REG_0,
             .imm = types[i] == BPF_PROG_TYPE_XDP ? 2 : 0},
            {.code = BPF_JMP | BPF_EXIT},
        };
        char license[] = "GPL", log[4096] = {};
        union bpf_attr load = {};
        load.prog_type = types[i];
        load.insn_cnt = 2;
        load.insns = (unsigned long)code;
        load.license = (unsigned long)license;
        load.log_buf = (unsigned long)log;
        load.log_size = sizeof(log);
        load.log_level = 1;
        fd = syscall(SYS_bpf, BPF_PROG_LOAD, &load, sizeof(load));
        if (fd < 0) fprintf(stderr, "BPF verifier: %s\n", log);
        CHECK(fd >= 0);
        char packet[64] = {};
        union bpf_attr run = {};
        run.test.prog_fd = fd;
        run.test.data_in = (unsigned long)packet;
        run.test.data_size_in = sizeof(packet);
        run.test.repeat = 1;
        CHECK(syscall(SYS_bpf, BPF_PROG_TEST_RUN, &run, sizeof(run)) == -1 && errno == EOPNOTSUPP);
        CHECK(close(fd) == 0);
    }
    puts("PASS_NATIVE_PACKET_GENERIC_AND_BPF_BOUNDARIES");
}
int main(int argc, char **argv)
{
    alarm(30);
    if (argc == 1) lifetime();
    else if (argc == 2 && !strcmp(argv[1], "native")) native_boundaries();
    else if (argc == 2 && !strcmp(argv[1], "managed")) managed();
    else if (argc == 2 && !strcmp(argv[1], "broker")) broker_failures();
    else CHECK(!"unknown namespace test mode");
    return 0;
}
