// SPDX-License-Identifier: GPL-2.0-only
fn main() {
    if let Err(error) = drv_network_service::run_provider() {
        eprintln!("netstack3-provider: {error}");
        std::process::exit(1);
    }
}
