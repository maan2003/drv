// SPDX-License-Identifier: GPL-2.0-only
// Disposable test-only tailnet using Tailscale v1.98.10 testcontrol and derpserver.
// No production accounts, auth keys, shared policy, or host routing changes.
// Control is host-loopback-only (QEMU SLIRP exposes it as 10.0.2.2).
// Relay accepts HTTP requests only from this host's own source address and uses
// certificate pinning. Never expose this automatic-registration controller.
package main

import (
	"crypto/sha256"
	"fmt"
	"log"
	"net"
	"net/http"
	"net/http/httptest"
	"strconv"

	"tailscale.com/derp/derpserver"
	"tailscale.com/tailcfg"
	"tailscale.com/tstest/integration/testcontrol"
	"tailscale.com/types/key"
)

func main() {
	c, err := net.Dial("udp4", "192.0.2.1:9")
	if err != nil {
		log.Fatal(err)
	}
	ip := c.LocalAddr().(*net.UDPAddr).IP.String()
	c.Close()
	h := derpserver.Handler(derpserver.New(key.NewNode(), log.Printf))
	relay := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		source, _, err := net.SplitHostPort(r.RemoteAddr)
		if err != nil || source != ip {
			http.Error(w, "lab host only", http.StatusForbidden)
			return
		}
		h.ServeHTTP(w, r)
	}))
	relay.Listener.Close()
	relay.Listener, err = net.Listen("tcp4", net.JoinHostPort(ip, "0"))
	if err != nil {
		log.Fatal(err)
	}
	relay.StartTLS()
	defer relay.Close()
	_, port, err := net.SplitHostPort(relay.Listener.Addr().String())
	if err != nil {
		log.Fatal(err)
	}
	p, err := strconv.Atoi(port)
	if err != nil {
		log.Fatal(err)
	}
	cert := fmt.Sprintf("sha256-raw:%x", sha256.Sum256(relay.Certificate().Raw))
	derpMap := &tailcfg.DERPMap{Regions: map[int]*tailcfg.DERPRegion{
		1: {RegionID: 1, RegionCode: "lab", RegionName: "Private lab", Nodes: []*tailcfg.DERPNode{
			{Name: "lab", RegionID: 1, HostName: "example.com", IPv4: ip, IPv6: "none", DERPPort: p, STUNPort: -1, CertName: cert},
		}},
	}}
	s := &testcontrol.Server{ExplicitBaseURL: "http://127.0.0.1:18766", AllNodesSameUser: true, AllOnline: true, DERPMap: derpMap}
	log.Fatal(http.ListenAndServe("127.0.0.1:18766", s))
}
