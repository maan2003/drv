// SPDX-License-Identifier: MIT OR Apache-2.0
#define _POSIX_C_SOURCE 200809L
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#define LINK_FD 3
#define NS3E_HEADER 8
#define RX_FRAME 3
#define TX_FRAME 4
#define DHCP_ENABLE "/run/netstack3-test-enable-dhcp"
#define LINK_DOWN "/run/netstack3-test-link-down"
#define GENERATIONS "/run/netstack3-test-link-generations"

static const uint8_t client_mac[6] = { 2, 0, 0, 0, 0, 1 };
static const uint8_t server_mac[6] = { 2, 0, 0, 0, 0, 2 };
static const uint8_t client_ip[4] = { 192, 0, 2, 10 };
static const uint8_t server_ip[4] = { 192, 0, 2, 2 };
static uint16_t ip_id;
static uint32_t server_isn = 1000;

static uint16_t get16(const uint8_t *p)
{
	return (uint16_t)p[0] << 8 | p[1];
}

static uint32_t get32(const uint8_t *p)
{
	return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 |
	       (uint32_t)p[2] << 8 | p[3];
}

static void put16(uint8_t *p, uint16_t v)
{
	p[0] = v >> 8; p[1] = v;
}

static void put32(uint8_t *p, uint32_t v)
{
	p[0] = v >> 24; p[1] = v >> 16; p[2] = v >> 8; p[3] = v;
}

static uint32_t checksum_add(uint32_t sum, const uint8_t *p, size_t len)
{
	while (len > 1) {
		sum += get16(p); p += 2; len -= 2;
	}
	if (len) sum += (uint16_t)p[0] << 8;
	return sum;
}

static uint16_t checksum_finish(uint32_t sum)
{
	while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16);
	return (uint16_t)~sum;
}

static uint16_t checksum(const uint8_t *p, size_t len)
{
	return checksum_finish(checksum_add(0, p, len));
}

static void send_packet(uint8_t opcode, const uint8_t *payload, uint16_t length)
{
	uint8_t packet[NS3E_HEADER + 1514];
	memcpy(packet, "NS3E", 4);
	packet[4] = 1; packet[5] = opcode;
	packet[6] = length; packet[7] = length >> 8;
	memcpy(packet + NS3E_HEADER, payload, length);
	if (send(LINK_FD, packet, NS3E_HEADER + length, 0) != NS3E_HEADER + length) {
		perror("send link packet"); exit(1);
	}
}

static void handshake(void)
{
	uint8_t attach[] = { 2, 0, 0, 0, 0, 1, 0xdc, 0x05 };
	uint8_t link_up = 1, ack[9];
	const uint8_t *payloads[] = { attach, &link_up };
	const uint16_t lengths[] = { sizeof(attach), sizeof(link_up) };
	for (uint8_t opcode = 1; opcode <= 2; opcode++) {
		send_packet(opcode, payloads[opcode - 1], lengths[opcode - 1]);
		if (recv(LINK_FD, ack, sizeof(ack), 0) != sizeof(ack) ||
		    memcmp(ack, "NS3E\1\5\1\0", 8) || ack[8] != opcode) {
			fprintf(stderr, "bad link acknowledgement\n"); exit(1);
		}
	}
}

static size_t ipv4(uint8_t *frame, uint8_t protocol, const uint8_t dst_mac[6],
		   const uint8_t src_ip[4], const uint8_t dst_ip[4], size_t body_len)
{
	uint8_t *ip = frame + 14;
	memcpy(frame, dst_mac, 6); memcpy(frame + 6, server_mac, 6);
	frame[12] = 8; frame[13] = 0;
	memset(ip, 0, 20);
	ip[0] = 0x45; put16(ip + 2, 20 + body_len); put16(ip + 4, ++ip_id);
	ip[8] = 64; ip[9] = protocol;
	memcpy(ip + 12, src_ip, 4); memcpy(ip + 16, dst_ip, 4);
	put16(ip + 10, checksum(ip, 20));
	return 14 + 20 + body_len;
}

static void send_arp(const uint8_t *request, size_t length)
{
	uint8_t frame[42] = { 0 };
	if (length < 42 || get16(request + 20) != 1 ||
	    memcmp(request + 38, server_ip, 4)) return;
	memcpy(frame, request + 6, 6); memcpy(frame + 6, server_mac, 6);
	frame[12] = 8; frame[13] = 6;
	put16(frame + 14, 1); put16(frame + 16, 0x0800);
	frame[18] = 6; frame[19] = 4; put16(frame + 20, 2);
	memcpy(frame + 22, server_mac, 6); memcpy(frame + 28, server_ip, 4);
	memcpy(frame + 32, request + 22, 6); memcpy(frame + 38, request + 28, 4);
	send_packet(RX_FRAME, frame, sizeof(frame));
}

static uint8_t dhcp_type(const uint8_t *bootp, size_t length)
{
	size_t i = 240;
	if (length < i || memcmp(bootp + 236, "\x63\x82\x53\x63", 4)) return 0;
	while (i < length) {
		uint8_t code = bootp[i++];
		if (code == 255) break;
		if (code == 0) continue;
		if (i >= length || i + bootp[i] >= length + 1) return 0;
		if (code == 53 && bootp[i] == 1) return bootp[i + 1];
		i += 1 + bootp[i];
	}
	return 0;
}

static void send_dhcp(const uint8_t *request, size_t length, uint8_t type)
{
	uint8_t frame[600] = { 0 }, *udp = frame + 34, *bootp = udp + 8, *o;
	size_t ip_hlen, bootp_len, total;
	if (length < 14 + 20 + 8 + 240) return;
	ip_hlen = (request[14] & 15) * 4;
	if (length < 14 + ip_hlen + 8 + 240) return;
	const uint8_t *incoming = request + 14 + ip_hlen + 8;
	memset(bootp, 0, 300);
	bootp[0] = 2; bootp[1] = 1; bootp[2] = 6;
	memcpy(bootp + 4, incoming + 4, 4);
	if (type != 6) memcpy(bootp + 16, client_ip, 4);
	if (type != 6) memcpy(bootp + 20, server_ip, 4);
	memcpy(bootp + 28, client_mac, 6);
	memcpy(bootp + 236, "\x63\x82\x53\x63", 4);
	o = bootp + 240;
	*o++ = 53; *o++ = 1; *o++ = type;
	*o++ = 54; *o++ = 4; memcpy(o, server_ip, 4); o += 4;
	if (type != 6) {
		*o++ = 51; *o++ = 4; put32(o, 20); o += 4;
		*o++ = 1; *o++ = 4; memcpy(o, "\xff\xff\xff\x00", 4); o += 4;
		*o++ = 6; *o++ = 4; memcpy(o, server_ip, 4); o += 4;
	}
	*o++ = 255;
	bootp_len = o - bootp;
	put16(udp, 67); put16(udp + 2, 68); put16(udp + 4, 8 + bootp_len);
	total = ipv4(frame, 17,
		     type == 6 ? request + 6 : (const uint8_t *)"\xff\xff\xff\xff\xff\xff",
		     server_ip,
		     type == 6 ? client_ip : (const uint8_t *)"\xff\xff\xff\xff",
		     8 + bootp_len);
	send_packet(RX_FRAME, frame, total);
}

static int handle_dhcp(const uint8_t *frame, size_t length, int respond)
{
	size_t ip_hlen;
	const uint8_t *udp, *bootp;
	uint8_t type;
	if (length < 14 + 20 + 8 || get16(frame + 12) != 0x0800 || frame[23] != 17)
		return 0;
	ip_hlen = (frame[14] & 15) * 4;
	if (length < 14 + ip_hlen + 8 || get16(frame + 14 + ip_hlen + 2) != 67)
		return 0;
	udp = frame + 14 + ip_hlen; bootp = udp + 8;
	type = dhcp_type(bootp, length - (bootp - frame));
	if (respond && type == 1) send_dhcp(frame, length, 2);
	else if (respond && type == 3) send_dhcp(frame, length, 5);
	else if (!respond && type == 3) send_dhcp(frame, length, 6);
	return type;
}

static void send_udp_echo(const uint8_t *request, size_t length)
{
	uint8_t frame[1514], *udp = frame + 34;
	size_t ip_hlen = (request[14] & 15) * 4, udp_len, total;
	const uint8_t *incoming = request + 14 + ip_hlen;
	if (length < 14 + ip_hlen + 8 || memcmp(request + 30, server_ip, 4)) return;
	udp_len = get16(incoming + 4);
	if (udp_len < 8 || length < 14 + ip_hlen + udp_len) return;
	put16(udp, get16(incoming + 2)); put16(udp + 2, get16(incoming));
	put16(udp + 4, udp_len); put16(udp + 6, 0);
	memcpy(udp + 8, incoming + 8, udp_len - 8);
	total = ipv4(frame, 17, request + 6, server_ip, request + 26, udp_len);
	send_packet(RX_FRAME, frame, total);
}

static uint16_t tcp_checksum(uint8_t *ip, uint8_t *tcp, size_t tcp_len)
{
	uint32_t sum = 6 + tcp_len;
	sum = checksum_add(sum, ip + 12, 8);
	sum = checksum_add(sum, tcp, tcp_len);
	return checksum_finish(sum);
}

static void send_tcp(const uint8_t *request, size_t length)
{
	uint8_t frame[1514], *ip = frame + 14, *tcp = frame + 34;
	size_t ip_hlen = (request[14] & 15) * 4, tcp_hlen, payload_len, tcp_len, total;
	const uint8_t *in = request + 14 + ip_hlen, *payload;
	uint32_t seq, ack, response_seq;
	uint8_t flags;
	if (length < 14 + ip_hlen + 20 || memcmp(request + 30, server_ip, 4)) return;
	tcp_hlen = (in[12] >> 4) * 4;
	if (tcp_hlen < 20 || length < 14 + ip_hlen + tcp_hlen) return;
	payload_len = length - 14 - ip_hlen - tcp_hlen; payload = in + tcp_hlen;
	seq = get32(in + 4); flags = in[13];
	if (flags & 2) {
		response_seq = ++server_isn * 1024;
		ack = seq + 1; flags = 0x12; payload_len = 0;
	} else if (payload_len) {
		response_seq = get32(in + 8);
		ack = seq + payload_len; flags = 0x18;
	} else {
		return;
	}
	memset(tcp, 0, 20); put16(tcp, get16(in + 2)); put16(tcp + 2, get16(in));
	put32(tcp + 4, response_seq); put32(tcp + 8, ack);
	tcp[12] = 5 << 4; tcp[13] = flags; put16(tcp + 14, 65535);
	if (payload_len) memcpy(tcp + 20, payload, payload_len);
	tcp_len = 20 + payload_len;
	total = ipv4(frame, 6, request + 6, server_ip, request + 26, tcp_len);
	put16(tcp + 16, tcp_checksum(ip, tcp, tcp_len));
	send_packet(RX_FRAME, frame, total);
}

static void handle_frame(const uint8_t *frame, size_t length)
{
	if (length < 14) return;
	if (get16(frame + 12) == 0x0806) { send_arp(frame, length); return; }
	if (get16(frame + 12) != 0x0800 || length < 34) return;
	if (frame[23] == 17) send_udp_echo(frame, length);
	else if (frame[23] == 6) send_tcp(frame, length);
}

int main(void)
{
	uint8_t packet[NS3E_HEADER + 1514], pending[1514];
	size_t pending_len = 0;
	struct timeval timeout = { .tv_sec = 0, .tv_usec = 100000 };
	int generations, link_down_sent = 0;
	if (!getenv("NETSTACK3_ETHERNET_FD") ||
	    strcmp(getenv("NETSTACK3_ETHERNET_FD"), "3")) return 2;
	if (setsockopt(LINK_FD, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout))) return 1;
	handshake();
	generations = open(GENERATIONS, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
	if (generations < 0) return 1;
	dprintf(generations, "%ld\n", (long)getpid()); close(generations);
	for (;;) {
		ssize_t n = recv(LINK_FD, packet, sizeof(packet), 0);
		if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
			int link_down = access(LINK_DOWN, F_OK) == 0;
			if (link_down && !link_down_sent) {
				uint8_t down = 0;
				send_packet(2, &down, sizeof(down)); link_down_sent = 1;
			} else if (!link_down) {
				link_down_sent = 0;
			}
			if (pending_len && access(DHCP_ENABLE, F_OK) == 0) {
				handle_dhcp(pending, pending_len, 1); pending_len = 0;
			}
			continue;
		}
		if (n <= 0) return n == 0 ? 0 : 1;
		if (n < NS3E_HEADER || memcmp(packet, "NS3E\1", 5) ||
		    packet[5] != TX_FRAME ||
		    (size_t)n != NS3E_HEADER + packet[6] + ((size_t)packet[7] << 8)) return 1;
		size_t length = n - NS3E_HEADER;
		uint8_t *frame = packet + NS3E_HEADER;
		if (access(LINK_DOWN, F_OK) == 0) continue;
		int dhcp_enabled = access(DHCP_ENABLE, F_OK) == 0;
		int type = handle_dhcp(frame, length, dhcp_enabled);
		if (type) {
			if (!dhcp_enabled && type == 1) {
				memcpy(pending, frame, length); pending_len = length;
			}
			continue;
		}
		handle_frame(frame, length);
	}
}
