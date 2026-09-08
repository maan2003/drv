// SPDX-License-Identifier: GPL-2.0-only

fn main() {
    if let Err(error) = wlan_softmac_host::netstack_child::run() {
        eprintln!("mt7921-netstack: {error}");
        std::process::exit(1);
    }
}
