// SPDX-License-Identifier: GPL-2.0-only
fn main() {
    let result = (|| {
        let mut args: Vec<_> = std::env::args().skip(1).collect();
        let bind_resolver = args.iter().any(|arg| arg == "--resolver");
        let inherited_resolver = args.iter().any(|arg| arg == "--resolver-fd");
        let resolver = match (bind_resolver, inherited_resolver) {
            (true, true) => return Err("resolver sources are mutually exclusive".into()),
            (true, false) => Some(drv_network_service::ResolverEndpoint::BindDefault),
            (false, true) => Some(drv_network_service::ResolverEndpoint::Inherited),
            (false, false) => None,
        };
        args.retain(|arg| arg != "--resolver" && arg != "--resolver-fd");
        let namespace = args.iter().any(|arg| arg == "--namespace");
        args.retain(|arg| arg != "--namespace");
        let netlink = args.iter().any(|arg| arg == "--netlink");
        args.retain(|arg| arg != "--netlink");
        let bootstrap = args.iter().any(|arg| arg == "--bootstrap");
        let link_control = args.iter().any(|arg| arg == "--link-control");
        args.retain(|arg| arg != "--bootstrap" && arg != "--link-control");
        let mac = match args.as_slice() {
            [] => None,
            [flag, value] if flag == "--ethernet-mac" => {
                let bytes: Result<Vec<_>, _> = value.split(':')
                    .map(|byte| u8::from_str_radix(byte, 16)).collect();
                let mac: [u8; 6] = bytes.map_err(|_| "invalid Ethernet MAC")?
                    .try_into().map_err(|_| "invalid Ethernet MAC")?;
                if mac == [0; 6] || mac[0] & 1 != 0 {
                    return Err("Ethernet MAC must be nonzero unicast".into());
                }
                Some(mac)
            }
            _ => return Err("usage: netstack3-provider [--ethernet-mac XX:XX:XX:XX:XX:XX] [--bootstrap] [--resolver | --resolver-fd] [--link-control] [--netlink] [--namespace] (registration FD3, initial frame FD4, bootstrap FD5, resolver FD7, link control FD8, netlink registration FD10, namespace control FD11)".into()),
        };
        drv_network_service::run_provider(mac, bootstrap, resolver, link_control, netlink, namespace)
    })();
    if let Err(error) = result {
        eprintln!("netstack3-provider: {error}");
        std::process::exit(1);
    }
}
