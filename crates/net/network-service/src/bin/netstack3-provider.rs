// SPDX-License-Identifier: GPL-2.0-only
fn main() {
    let result = (|| {
        let mut args: Vec<_> = std::env::args().skip(1).collect();
        let resolver = args.iter().any(|arg| arg == "--resolver");
        args.retain(|arg| arg != "--resolver");
        let bootstrap = args.last().is_some_and(|arg| arg == "--bootstrap");
        if bootstrap { args.pop(); }
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
            _ => return Err("usage: netstack3-provider [--ethernet-mac XX:XX:XX:XX:XX:XX] [--bootstrap] [--resolver] (frame capability on FD4, bootstrap on FD5)".into()),
        };
        drv_network_service::run_provider(mac, bootstrap, resolver)
    })();
    if let Err(error) = result {
        eprintln!("netstack3-provider: {error}");
        std::process::exit(1);
    }
}
