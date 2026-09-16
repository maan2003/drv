/* SPDX-License-Identifier: GPL-2.0-only */
#define _GNU_SOURCE
#include <sys/socket.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <linux/netlink.h>
#include <linux/rtnetlink.h>
#include <linux/genetlink.h>
#include <sched.h>
#include <poll.h>
#include <pthread.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <endian.h>
#include "netlink-protocol.h"
#include "namespace-protocol.h"
static void check(int ok, const char *what) {
    if (!ok) { fprintf(stderr,"FAIL %s errno=%d\n",what,errno); exit(1); }
}
static int registration(void) {
    int fd=open("/dev/netstack3",O_RDWR|O_CLOEXEC);
    check(fd>=0,"registration");
    check(ioctl(fd,NS3_NAMESPACE_READY)==0,"ready"); return fd;
}
static int client(unsigned groups) {
    int fd=socket(AF_NETLINK,SOCK_RAW|SOCK_NONBLOCK,NETLINK_ROUTE);
    struct sockaddr_nl a={.nl_family=AF_NETLINK,.nl_groups=groups};
    check(fd>=0 && bind(fd,(void*)&a,sizeof(a))==0,"client bind");
    return fd;
}
static unsigned port(int fd) {
    struct sockaddr_nl a; socklen_t n=sizeof(a);
    check(getsockname(fd,(void*)&a,&n)==0,"port"); return a.nl_pid;
}
static int claim(int reg) {
    uint64_t id=0; int fd=ioctl(reg,NS3_NL_CLAIM,&id);
    check(fd>=0 && id!=0,"claim"); return fd;
}
static void send_request(int fd) {
    struct nlmsghdr n={.nlmsg_len=sizeof(n),.nlmsg_type=RTM_GETLINK,
        .nlmsg_flags=NLM_F_REQUEST,.nlmsg_seq=17,.nlmsg_pid=0x12345678};
    struct sockaddr_nl dst={.nl_family=AF_NETLINK};
    check(sendto(fd,&n,sizeof(n),0,(void*)&dst,sizeof(dst))==sizeof(n),"send");
}
static void receive_request(int fd,unsigned expected_port) {
    uint8_t b[128]; ssize_t n=read(fd,b,sizeof(b));
    check(n==sizeof(struct ns3_nl_record)+sizeof(struct nlmsghdr),"request record");
    struct ns3_nl_record *r=(void*)b;
    check(le32toh(r->version)==NS3_NL_VERSION &&
          le32toh(r->kind)==NS3_NL_REQUEST &&
          le32toh(r->portid)==expected_port,"authenticated port, not payload pid");
}
static void reply(int fd,unsigned group) {
    struct { uint32_t group,reserved; struct nlmsghdr n; } b={
        .group=htole32(group),.n={.nlmsg_len=sizeof(struct nlmsghdr),
        .nlmsg_type=NLMSG_DONE,.nlmsg_seq=17}};
    check(write(fd,&b,sizeof(b))==sizeof(b),"reply or scoped multicast");
}
static int ready(int fd,short events) {
    struct pollfd p={.fd=fd,.events=events};
    check(poll(&p,1,0)>=0,"poll"); return p.revents;
}
static void receive_reply(int fd,unsigned group) {
    struct nlmsghdr n;
    struct sockaddr_nl src; socklen_t len=sizeof(src);
    check(recvfrom(fd,&n,sizeof(n),0,(void*)&src,&len)==sizeof(n),"receive");
    check(src.nl_pid==0 && src.nl_groups==(group ? 1U<<(group-1) : 0),"kernel identity");
}
static void *blocked_receive(void *ptr) {
    int fd=*(int*)ptr; struct nlmsghdr n;
    int rc=recv(fd,&n,sizeof(n),0);
    check(rc<=0,"death wakes blocked receiver"); return NULL;
}
int main(void) {
    alarm(15);
    int reg=registration();
    check(open("/dev/netstack3",O_RDWR)==-1 && errno==EBUSY,"exclusive registration");
    int a=client(0),b=client(RTMGRP_LINK);
    int ea=claim(reg),eb=claim(reg);
    check(ioctl(ea,NS3_NL_CLAIM,&(uint64_t){0})==-1 && errno==ENOTTY,"typed endpoint");
    send_request(a); receive_request(ea,port(a));
    check(!ready(eb,POLLIN),"per-socket requests");
    reply(ea,0); receive_reply(a,0);
    check(!ready(b,POLLIN),"per-FD replies");
    reply(reg,RTNLGRP_LINK); receive_reply(b,RTNLGRP_LINK);
    check(!ready(a,POLLIN),"subscription isolation");
    int group=RTNLGRP_LINK;
    check(setsockopt(b,SOL_NETLINK,NETLINK_DROP_MEMBERSHIP,&group,sizeof(group))==0,"unsubscribe");
    reply(reg,RTNLGRP_LINK); check(!ready(b,POLLIN),"unsubscribe applied");
    check(setsockopt(a,SOL_NETLINK,NETLINK_ADD_MEMBERSHIP,&group,sizeof(group))==0,"subscribe");
    reply(reg,RTNLGRP_LINK); receive_reply(a,RTNLGRP_LINK);
    /* Subscription-only sockets receive even before the provider claims them. */
    int c=client(RTMGRP_LINK);
    reply(reg,RTNLGRP_LINK); receive_reply(a,RTNLGRP_LINK); receive_reply(c,RTNLGRP_LINK);
    int ec=claim(reg);
    for(int i=0;i<32;i++) send_request(a);
    struct sockaddr_nl dst={.nl_family=AF_NETLINK};
    struct nlmsghdr n={.nlmsg_len=sizeof(n),.nlmsg_type=RTM_GETLINK};
    check(sendto(a,&n,sizeof(n),0,(void*)&dst,sizeof(dst))==-1 && errno==EAGAIN,"bounded TX");
    check(!(ready(a,POLLOUT)&POLLOUT),"full queue suppresses writable");
    receive_request(ea,port(a));
    check(ready(a,POLLOUT)&POLLOUT,"dequeue restores writable");
    send_request(b); receive_request(eb,port(b)); reply(eb,0); receive_reply(b,0);
    /* A slow subscriber cannot stall or silently corrupt a fast one. */
    int slow=client(RTMGRP_LINK), fast=client(RTMGRP_LINK);
    int small=4096;
    check(!setsockopt(slow,SOL_SOCKET,SO_RCVBUF,&small,sizeof(small)),"small RX");
    for(int i=0;i<64;i++) {
        reply(reg,RTNLGRP_LINK);
        receive_reply(a,RTNLGRP_LINK);
        receive_reply(c,RTNLGRP_LINK);
        receive_reply(fast,RTNLGRP_LINK);
    }
    int error=0; socklen_t error_len=sizeof(error);
    check(!getsockopt(slow,SOL_SOCKET,SO_ERROR,&error,&error_len) && error==ENOBUFS,"multicast loss reported");
    close(slow); close(fast);
    /* Sender credentials come from the syscall, not socket creator/header. */
    int unpriv=client(0), eunpriv=claim(reg);
    pid_t pid=fork(); check(pid>=0,"credential fork");
    if(!pid) {
        check(!setgid(65534) && !setuid(65534),"drop credentials");
        check(open("/dev/netstack3",O_RDWR)<0,"unprivileged registration denied");
        send_request(unpriv); _exit(0);
    }
    int status; check(waitpid(pid,&status,0)==pid && WIFEXITED(status) && !WEXITSTATUS(status),"credential child");
    uint8_t record[128];
    check(read(eunpriv,record,sizeof(record))==40+sizeof(struct nlmsghdr),"credential request");
    struct ns3_nl_record *context=(void*)record;
    check(le32toh(context->uid)==65534 && le32toh(context->gid)==65534 &&
          le32toh(context->net_admin)==0,"authenticated per-send credentials");
    close(unpriv); close(eunpriv);
    /* Another namespace has an independent registration and socket population. */
    pid=fork(); check(pid>=0,"namespace fork");
    if(!pid) {
        check(!unshare(CLONE_NEWNET),"new network namespace");
        int other=registration(), app=client(0), endpoint=claim(other);
        send_request(app); receive_request(endpoint,port(app));
        reply(endpoint,0); receive_reply(app,0);
        close(app); close(endpoint); close(other); _exit(0);
    }
    check(waitpid(pid,&status,0)==pid && WIFEXITED(status) && !WEXITSTATUS(status),"namespace isolation");
    int generic=socket(AF_NETLINK,SOCK_RAW,NETLINK_GENERIC);
    int uevent=socket(AF_NETLINK,SOCK_DGRAM,NETLINK_KOBJECT_UEVENT);
    check(generic>=0 && uevent>=0,"unrelated netlink unchanged");
    struct {
        struct nlmsghdr n;
        struct genlmsghdr g;
        struct nlattr a;
        char name[8];
    } ctrl={
        .n={.nlmsg_len=32,.nlmsg_type=GENL_ID_CTRL,.nlmsg_flags=NLM_F_REQUEST,.nlmsg_seq=99},
        .g={.cmd=CTRL_CMD_GETFAMILY,.version=1},
        .a={.nla_len=11,.nla_type=CTRL_ATTR_FAMILY_NAME},.name="nlctrl"
    };
    check(sendto(generic,&ctrl,sizeof(ctrl),0,(void*)&dst,sizeof(dst))==sizeof(ctrl),"native generic request");
    uint8_t generic_reply[4096];
    int received=recv(generic,generic_reply,sizeof(generic_reply),0);
    check(received>20 && ((struct nlmsghdr*)generic_reply)->nlmsg_type==GENL_ID_CTRL,
          "native generic family discovery preserved");
    close(generic); close(uevent);
    int flags=fcntl(b,F_GETFL);
    check(fcntl(b,F_SETFL,flags&~O_NONBLOCK)==0,"blocking reader");
    pthread_t thread; check(!pthread_create(&thread,NULL,blocked_receive,&b),"reader");
    usleep(20000);
    close(reg);
    check(!pthread_join(thread,NULL),"join reader");
    check(ready(a,POLLIN)&(POLLERR|POLLHUP),"death wakes app poll");
    check(ready(ea,POLLIN)&(POLLERR|POLLHUP),"death wakes provider poll");
    check(socket(AF_NETLINK,SOCK_RAW,NETLINK_ROUTE)==-1 && errno==ENETDOWN,"no native fallback");
    reg=registration();
    uint8_t out[32]={0};
    check(write(ea,out,sizeof(out))==-1 && errno==ENETDOWN,"no resurrection");
    int fresh=client(0), efresh=claim(reg);
    send_request(fresh); receive_request(efresh,port(fresh)); reply(efresh,0); receive_reply(fresh,0);
    close(a); close(b); close(c); close(ea); close(eb); close(ec);
    close(fresh); close(efresh); close(reg);
    puts("PASS_NETLINK_BOUNDARY");
}
