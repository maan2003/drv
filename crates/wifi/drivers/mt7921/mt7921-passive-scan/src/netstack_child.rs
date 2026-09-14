// SPDX-License-Identifier: GPL-2.0-only

fn main() {
    if let Err(error) = drv_network_service::run_lab() {
        eprintln!("mt7921-netstack: {error}");
        std::process::exit(1);
    }
}
