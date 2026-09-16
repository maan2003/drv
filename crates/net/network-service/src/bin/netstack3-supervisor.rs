// SPDX-License-Identifier: GPL-2.0-only
fn main() {
    let mut args = std::env::args_os().skip(1);
    let result = match (args.next(), args.next()) {
        (Some(provider), None) => drv_network_service::run_namespace_supervisor(provider),
        _ => Err("usage: netstack3-supervisor /absolute/path/to/netstack3-provider".into()),
    };
    if let Err(error) = result {
        eprintln!("netstack3-supervisor: {error}");
        std::process::exit(1);
    }
}
