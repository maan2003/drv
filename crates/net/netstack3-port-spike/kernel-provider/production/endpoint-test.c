// SPDX-License-Identifier: GPL-2.0-only
#define _GNU_SOURCE
#include <sys/socket.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <sys/resource.h>
#include <sys/mman.h>
#include <arpa/inet.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <poll.h>
#include <time.h>
#include "protocol.h"
static void check(int ok, const char *what) {
    if (!ok) { fprintf(stderr,"FAIL endpoint %s errno=%d\n",what,errno); exit(1); }
}
struct frame { struct ns3_msg h; unsigned char data[NS3_PAYLOAD+24]; };
static struct ns3_addr addr(unsigned family, unsigned port) {
    struct ns3_addr a = {.family=family,.port=port};
    if (family==4) { a.address[0]=127; a.address[3]=1; } else a.address[15]=1;
    return a;
}
static int claim(int registration) {
    unsigned long long id;
    int fd=ioctl(registration,NS3_CLAIM,&id);
    check(fd>=0 && (fcntl(fd,F_GETFD)&FD_CLOEXEC),"claim scoped CLOEXEC endpoint");
    return fd;
}
static struct frame control(int fd) {
    struct frame f={0};
    for(int i=0;i<2000;i++) {
        int n=ioctl(fd,NS3_READ_CONTROL,&f);
        if(n>=0) {
            check(n==(int)(sizeof(f.h)+f.h.len) && f.h.version==NS3_VERSION,"control framing");
            return f;
        }
        check(errno==EAGAIN,"control read"); usleep(1000);
    }
    check(0,"control timeout"); return f;
}
static void reply(int fd, struct frame *f, unsigned status, const void *data, size_t n) {
    f->h.status=status; f->h.len=n;
    if(n) memcpy(f->data,data,n);
    check(write(fd,f,sizeof(f->h)+n)==(ssize_t)(sizeof(f->h)+n),"typed completion");
}
static void opened(int fd) {
    struct frame f=control(fd);
    check(f.h.op==NS3_OPEN,"OPEN precedes operations");
    unsigned family; memcpy(&family,f.data+4,4);
    struct ns3_addr a=addr(family,0);
    reply(fd,&f,0,&a,sizeof(a));
}
static int app_socket(int type) {
    int fd=socket(AF_INET,type,0); check(fd>=0,"app socket"); return fd;
}
static void inject(int fd, unsigned n) {
    struct frame f={.h={.version=NS3_VERSION,.op=NS3_RX,.len=24+n}};
    struct ns3_addr a=addr(4,20001); memcpy(f.data,&a,24);
    memset(f.data+24,'x',n);
    check(write(fd,&f,sizeof(f.h)+f.h.len)==(ssize_t)(sizeof(f.h)+f.h.len),"RX admission");
}
static struct sockaddr_in dest={.sin_family=AF_INET,.sin_port=0};
static void prepare_udp(int app,int ep) {
    errno=0;
    check(sendto(app,"x",1,MSG_DONTWAIT,(void*)&dest,sizeof(dest))<0 && errno==EAGAIN,
          "lazy activation DONTWAIT consumes no payload");
    opened(ep);
    struct frame f=control(ep); check(f.h.op==NS3_ACTIVATE,"ACTIVATE");
    struct ns3_addr a=addr(4,20002); reply(ep,&f,0,&a,sizeof(a));
    struct sockaddr_in name; socklen_t len=sizeof(name);
    check(getsockname(app,(void*)&name,&len)==0 && name.sin_port==htons(20002),"local committed metadata");
    struct pollfd p={.fd=app,.events=POLLOUT};
    check(poll(&p,1,0)==1 && p.revents&POLLOUT,"activation wakes writable");
}
static void signal_noop(int n) { (void)n; }
static void child_ok(pid_t p,const char *what) {
    int status; check(waitpid(p,&status,0)==p && WIFEXITED(status) && !WEXITSTATUS(status),what);
}
int main(void) {
    alarm(30); setbuf(stdout,NULL);
    dest.sin_port=htons(20001); dest.sin_addr.s_addr=htonl(0x7f000001);
    int registration=open("/dev/netstack3",O_RDWR|O_CLOEXEC);
    check(registration>=0,"registration");
    int a=app_socket(SOCK_DGRAM|SOCK_NONBLOCK);
    check(ioctl(registration,NS3_CLAIM,(void*)1)<0 && errno==EFAULT,"claim copy rollback");
    struct rlimit original, limited;
    check(getrlimit(RLIMIT_NOFILE,&original)==0,"fd limit"); limited=original; limited.rlim_cur=64;
    check(setrlimit(RLIMIT_NOFILE,&limited)==0,"limit fds");
    int fillers[64],count=0; while(count<64) { int fd=dup(registration); if(fd<0)break; fillers[count++]=fd; }
    unsigned long long id;
    check(ioctl(registration,NS3_CLAIM,&id)<0 && errno==EMFILE,"claim reservation rollback");
    while(count)close(fillers[--count]);
    check(setrlimit(RLIMIT_NOFILE,&original)==0,"restore limit");
    int ep=claim(registration);
    prepare_udp(a,ep);
    puts("PASS ABI6_CLAIM_ROLLBACK_LAZY_ACTIVATION_METADATA");

    char bytes[NS3_PAYLOAD]={0}; size_t admitted=0;
    while(sendto(a,bytes,sizeof(bytes),0,(void*)&dest,sizeof(dest))==(ssize_t)sizeof(bytes)) {
        admitted+=sizeof(bytes); check(admitted<=256*1024,"bounded TX bytes");
    }
    check(errno==EAGAIN && admitted==256*1024,"TX occupancy admission");
    void *fault=mmap(0,sizeof(struct frame),PROT_NONE,MAP_PRIVATE|MAP_ANONYMOUS,-1,0);
    check(fault!=MAP_FAILED,"fault mapping");
    check(read(ep,fault,sizeof(struct frame))<0 && errno==EFAULT,"TX copy fault rollback");
    munmap(fault,sizeof(struct frame));
    struct frame f;
    check(read(ep,&f,sizeof(f))==(ssize_t)(sizeof(f.h)+24+sizeof(bytes)) && f.h.op==NS3_SEND && !f.h.request,
          "data ownership transfer has no request ID");
    check(sendto(a,bytes,sizeof(bytes),0,(void*)&dest,sizeof(dest))==(ssize_t)sizeof(bytes),
          "dequeue alone restores admission without completion");
    admitted+=sizeof(bytes);
    close(a); f=control(ep);
    check(f.h.op==NS3_CLOSE && f.h.len==8,"allocation-free close lane under TX saturation");
    unsigned long long seal; memcpy(&seal,f.data,8);
    check(seal==admitted/NS3_PAYLOAD,"UDP seal counts records");
    int drained=1;
    while(read(ep,&f,sizeof(f))>0)drained++;
    check(errno==EAGAIN && drained==(int)seal,"drain through close seal");
    close(ep);
    puts("PASS ABI6_TX_BOUND_COPY_TRANSACTION_SEAL_NO_REPLIES");

    a=app_socket(SOCK_DGRAM|SOCK_NONBLOCK); ep=claim(registration);
    for(int i=0;i<32;i++)inject(ep,0);
    f=(struct frame){.h={.version=NS3_VERSION,.op=NS3_RX,.len=24}};
    struct ns3_addr source=addr(4,1); memcpy(f.data,&source,24);
    check(write(ep,&f,sizeof(f.h)+24)<0 && errno==EAGAIN,"zero records bounded");
    check(recv(a,bytes,1,MSG_PEEK)==0,"zero record peek");
    check(write(ep,&f,sizeof(f.h)+24)<0 && errno==EAGAIN,"peek retains occupancy");
    check(recv(a,bytes,1,0)==0,"consume zero record");
    inject(ep,0); // no credit-return message needed
    close(a);close(ep);
    puts("PASS ABI6_RX_OCCUPANCY_ZERO_RECORDS_NO_CREDITS");

    // Shared-FD blocked recv must not prevent DONTWAIT or signal delivery.
    a=app_socket(SOCK_DGRAM); ep=claim(registration);
    pid_t waiter=fork(); check(waiter>=0,"recv fork");
    if(!waiter) { struct sigaction sa={.sa_handler=signal_noop}; sigaction(SIGUSR1,&sa,0);
        int n=recv(a,bytes,1,0); _exit(!(n<0&&errno==EINTR)); }
    usleep(30000);
    check(recv(a,bytes,1,MSG_DONTWAIT)<0 && errno==EAGAIN,"shared FD nonblocking contender");
    kill(waiter,SIGUSR1); child_ok(waiter,"blocked recv remains interruptible");
    struct timeval timeout={.tv_usec=30000};
    check(setsockopt(a,SOL_SOCKET,SO_RCVTIMEO,&timeout,sizeof(timeout))==0,"receive deadline");
    check(recv(a,bytes,1,0)<0 && errno==EAGAIN,"finite receive deadline");
    close(a);close(ep);
    puts("PASS ABI6_SHARED_FD_RECV_DONTWAIT_SIGNAL_DEADLINE");

    // TCP queue contention uses the same state transition waits, not a
    // syscall-long transmitter mutex. A short provider read commits a prefix.
    a=app_socket(SOCK_STREAM|SOCK_NONBLOCK);ep=claim(registration);
    check(connect(a,(void*)&dest,sizeof(dest))<0&&errno==EINPROGRESS,"TCP connect pending");
    opened(ep);f=control(ep);check(f.h.op==NS3_CONNECT,"TCP connect ack");
    struct ns3_addr names[2]={addr(4,20100),addr(4,20001)};
    reply(ep,&f,0,names,sizeof(names));
    f.h.op=NS3_CONNECTION;reply(ep,&f,0,names,sizeof(names));
    check(fcntl(a,F_SETFL,0)==0,"blocking shared TCP descriptor");
    size_t total=0;
    while(send(a,bytes,sizeof(bytes),MSG_DONTWAIT|MSG_NOSIGNAL)==sizeof(bytes))total+=sizeof(bytes);
    check(errno==EAGAIN&&total==256*1024,"saturate TCP queue");
    waiter=fork();check(waiter>=0,"send fork");
    if(!waiter) {struct sigaction sa={.sa_handler=signal_noop};sigaction(SIGUSR1,&sa,0);
        int n=send(a,"x",1,MSG_NOSIGNAL);_exit(!(n<0&&errno==EINTR));}
    usleep(30000);
    check(send(a,"x",1,MSG_DONTWAIT|MSG_NOSIGNAL)<0&&errno==EAGAIN,"shared FD send DONTWAIT contender");
    kill(waiter,SIGUSR1);child_ok(waiter,"blocked send interruptible");
    check(setsockopt(a,SOL_SOCKET,SO_SNDTIMEO,&timeout,sizeof(timeout))==0,"send deadline");
    check(send(a,"x",1,MSG_NOSIGNAL)<0&&errno==EAGAIN,"finite send deadline");
    check(read(ep,&f,sizeof(f.h)+25)==(ssize_t)(sizeof(f.h)+25)&&f.h.len==25,"partial TCP dequeue");
    close(a);f=control(ep);memcpy(&seal,f.data,8);
    check(f.h.op==NS3_CLOSE&&seal==total,"TCP seal counts bytes");
    close(ep);
    puts("PASS ABI6_SHARED_FD_SEND_DONTWAIT_SIGNAL_DEADLINE_PARTIAL_DEQUEUE");

    a=app_socket(SOCK_DGRAM);ep=claim(registration);
    pid_t first=fork();check(first>=0,"first control contender");
    if(!first) {_exit(bind(a,(void*)&dest,sizeof(dest))!=0);}
    opened(ep);f=control(ep);check(f.h.op==NS3_BIND,"first control dispatched");
    pid_t second=fork();check(second>=0,"second control contender");
    if(!second) {struct sigaction sa={.sa_handler=signal_noop};sigaction(SIGUSR1,&sa,0);
        int n=bind(a,(void*)&dest,sizeof(dest));_exit(!(n<0&&errno==EINTR));}
    usleep(30000);
    check(sendto(a,"x",1,MSG_DONTWAIT,(void*)&dest,sizeof(dest))<0&&errno==EAGAIN,
          "nonblocking send behind pending bind");
    kill(second,SIGUSR1);child_ok(second,"control slot contender interruptible");
    source=addr(4,20001);reply(ep,&f,0,&source,24);child_ok(first,"first control survives contender signal");
    close(a);close(ep);
    puts("PASS ABI6_CONTROL_SLOT_CONTENTION");

    a=app_socket(SOCK_DGRAM);ep=claim(registration);
    check(setsockopt(a,SOL_SOCKET,SO_RCVTIMEO,&timeout,sizeof(timeout))==0,"deadline flood setup");
    pid_t flood=fork();check(flood>=0,"deadline flood fork");
    if(!flood) {
        struct ns3_msg wake={.version=NS3_VERSION,.op=NS3_STATE,.len=4};
        unsigned char record[sizeof(wake)+4];memcpy(record,&wake,sizeof(wake));memset(record+sizeof(wake),0,4);
        for(int i=0;i<2000;i++) { if(write(ep,record,sizeof(record))<0)break; usleep(100); }
        _exit(0);
    }
    struct timespec begin,end;clock_gettime(CLOCK_MONOTONIC,&begin);
    check(recv(a,bytes,1,0)<0&&errno==EAGAIN,"deadline expires amid wakeups");
    clock_gettime(CLOCK_MONOTONIC,&end);
    long elapsed=(end.tv_sec-begin.tv_sec)*1000000+(end.tv_nsec-begin.tv_nsec)/1000;
    check(elapsed<150000,"wakeups cannot repeatedly extend total deadline");
    kill(flood,SIGKILL);waitpid(flood,0,0);close(a);close(ep);
    puts("PASS ABI6_ABSOLUTE_DEADLINE_WAKE_FLOOD");

    // Malformed successful mutation is terminal, before any local publication.
    for(int wrong_family=0;wrong_family<2;wrong_family++) {
        a=app_socket(SOCK_DGRAM);ep=claim(registration);
        pid_t binder=fork();check(binder>=0,"bind fork");
        if(!binder) { int n=bind(a,(void*)&dest,sizeof(dest)); _exit(!(n<0&&errno==ENETDOWN)); }
        opened(ep);f=control(ep);check(f.h.op==NS3_BIND,"bind dispatch");
        f.h.len=wrong_family?24:1;
        struct ns3_addr bad=addr(6,1);memcpy(f.data,&bad,24);
        check(write(ep,&f,sizeof(f.h)+f.h.len)<0&&errno==EPROTO,"reject malformed typed mutation");
        child_ok(binder,"malformed mutation wakes revoked waiter");
        close(a);close(ep);
    }
    puts("PASS ABI6_MALFORMED_MUTATION_FAMILY_REVOKES");

    a=app_socket(SOCK_DGRAM);ep=claim(registration);
    pid_t binder=fork();check(binder>=0,"cancel fork");
    if(!binder) { struct sigaction sa={.sa_handler=signal_noop};sigaction(SIGUSR1,&sa,0);
        int n=bind(a,(void*)&dest,sizeof(dest));_exit(!(n<0&&errno==EINTR)); }
    opened(ep);f=control(ep);check(f.h.op==NS3_BIND,"cancel dispatched bind");
    kill(binder,SIGUSR1);child_ok(binder,"dispatched control signal");
    check(recv(a,bytes,1,MSG_DONTWAIT)<0&&errno==ENETDOWN,"ambiguous cancellation revokes");
    close(a);close(ep);
    puts("PASS ABI6_CONTROL_CANCELLATION");

    // A completed control cannot be completed twice.
    a=app_socket(SOCK_DGRAM|SOCK_NONBLOCK);ep=claim(registration);
    prepare_udp(a,ep);
    f=(struct frame){.h={.version=NS3_VERSION,.op=NS3_ACTIVATE,.request=2,.len=24}};
    source=addr(4,20002);memcpy(f.data,&source,24);
    check(write(ep,&f,sizeof(f.h)+24)<0&&errno==EPROTO,"duplicate completion revokes");
    check(recv(a,bytes,1,0)<0&&errno==ENETDOWN,"duplicate terminal disposition");
    close(a);close(ep);
    int held[300], retained=0;
    for (; retained<300; retained++) {
        int app=socket(AF_INET,SOCK_DGRAM,0);
        if(app<0) { check(errno==ENFILE,"retained endpoint quota"); break; }
        held[retained]=claim(registration);
        close(app);
    }
    check(retained==256,"closed application charged while provider retains endpoint");
    for(int i=0;i<retained;i++)close(held[i]);
    int recovered=app_socket(SOCK_DGRAM);
    close(recovered);
    puts("PASS ABI6_ENDPOINT_LIFETIME_QUOTA");
    close(registration);
    puts("PASS ABI6_ENDPOINT_SUITE");
    return 0;
}
