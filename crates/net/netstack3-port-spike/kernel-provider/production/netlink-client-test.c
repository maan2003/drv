/* SPDX-License-Identifier: GPL-2.0-only */
#define _GNU_SOURCE
#include <ifaddrs.h>
#include <netdb.h>
#include <net/if.h>
#include <sys/socket.h>
#include <linux/netlink.h>
#include <linux/rtnetlink.h>
#include <poll.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static void check(int ok,const char *s) { if(!ok){perror(s);exit(1);} }
int main(int argc,char **argv) {
    alarm(10);
    if(argc==2 && !strcmp(argv[1],"watch")) {
        int fd=socket(AF_NETLINK,SOCK_RAW,NETLINK_ROUTE);
        struct sockaddr_nl a={.nl_family=AF_NETLINK,.nl_groups=RTMGRP_IPV4_IFADDR};
        check(fd>=0 && !bind(fd,(void*)&a,sizeof(a)),"watch bind");
        puts("WATCH_READY");fflush(stdout);
        for(;;) {
            unsigned char b[65536];
            int len=recv(fd,b,sizeof(b),0);
            check(len>0,"watch recv");
            struct nlmsghdr *n=(void*)b;
            for(;NLMSG_OK(n,len);n=NLMSG_NEXT(n,len))
                if(n->nlmsg_type==RTM_DELADDR) {
                    return 0;
                }
        }
    }
    check(argc==2,"online/offline argument");
    int expected=!strcmp(argv[1],"online"),v4=0,v6=0,loopback=0,ethernet=0;
    struct ifaddrs *list;
    check(!getifaddrs(&list),"getifaddrs");
    for(struct ifaddrs *i=list;i;i=i->ifa_next) {
        if(!i->ifa_addr)continue;
        if(!strcmp(i->ifa_name,"lo"))loopback=1;
        if(!strcmp(i->ifa_name,"netstack0"))ethernet=1;
        if(i->ifa_flags&IFF_LOOPBACK)continue;
        v4|=i->ifa_addr->sa_family==AF_INET;
        v6|=i->ifa_addr->sa_family==AF_INET6;
    }
    freeifaddrs(list);
    check(loopback&&ethernet,"real interface enumeration");
    check(v4==expected,"DHCP address tracks link state");
    struct addrinfo hints={.ai_family=AF_INET,.ai_socktype=SOCK_STREAM,
        .ai_flags=AI_ADDRCONFIG|AI_NUMERICHOST},*out=NULL;
    int e=getaddrinfo("192.0.2.1",NULL,&hints,&out);
    check((e==0)==v4,"AI_ADDRCONFIG IPv4");
    if(!e)freeaddrinfo(out);
    hints.ai_family=AF_INET6;
    e=getaddrinfo("2001:db8::1",NULL,&hints,&out);
    check((e==0)==v6,"AI_ADDRCONFIG IPv6");
    if(!e)freeaddrinfo(out);
    puts("PASS_NETLINK_GLIBC_DISCOVERY");
}
