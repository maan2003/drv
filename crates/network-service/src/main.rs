// SPDX-License-Identifier: GPL-2.0-only

fn main() {
    if let Err(error) = drv_network_service::run() {
        eprintln!("drv-network-service: {error}");
        std::process::exit(1);
    }
}
