// SPDX-License-Identifier: GPL-2.0-only
// Build with CGO_ENABLED=0: exercises the same standard-library discovery
// path used by the deployed pure-Go Tailscale, independently of glibc.
package main

import (
    "fmt"
    "net"
    "os"
)

func main() {
    if len(os.Args) != 2 { panic("online/offline argument required") }
    interfaces, err := net.Interfaces()
    if err != nil { panic(err) }
    found, loopback, v4 := false, false, false
    for _, iface := range interfaces {
        if iface.Name == "lo" { loopback = true }
        if iface.Name != "netstack0" { continue }
        found = true
        if (iface.Flags & net.FlagUp != 0) != (os.Args[1] == "online") {
            panic("link flags do not track carrier")
        }
        addresses, err := iface.Addrs()
        if err != nil { panic(err) }
        for _, address := range addresses {
            ip, _, err := net.ParseCIDR(address.String())
            if err != nil { panic(err) }
            v4 = v4 || (ip.To4() != nil && !ip.IsLoopback())
        }
    }
    if !found || !loopback || v4 != (os.Args[1] == "online") {
        panic("interface/address view does not track real DHCP/link state")
    }
    fmt.Println("PASS_NETLINK_PURE_GO_DISCOVERY")
}
