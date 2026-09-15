// SPDX-License-Identifier: GPL-2.0-only
fn main() {
    let result = (|| {
        let mut args: Vec<_> = std::env::args().skip(1).collect();
        let resolver = args.iter().any(|arg| arg == "--resolver");
        args.retain(|arg| arg != "--resolver");
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
            _ => return Err("usage: netstack3-provider [--ethernet-mac XX:XX:XX:XX:XX:XX] [--bootstrap] [--resolver] [--link-control] (registration FD3, reserved frame FD4, bootstrap FD5, link control FD8)".into()),
        };
        drv_network_service::run_provider(mac, bootstrap, resolver, link_control)
    })();
    if let Err(error) = result {
        eprintln!("netstack3-provider: {error}");
        std::process::exit(1);
    }
}
