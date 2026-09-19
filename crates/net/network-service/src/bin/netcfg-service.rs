// SPDX-License-Identifier: GPL-2.0-only

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mac = args.next().ok_or("usage: netcfg-service MAC PROVIDER_GENERATION")?;
    let mac = mac.split(':').map(|v| u8::from_str_radix(v, 16))
        .collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    let generation = args.next().ok_or("missing provider generation")?
        .parse().map_err(|_| "invalid provider generation")?;
    if args.next().is_some() { return Err("unexpected netcfg argument".into()); }
    drv_network_service::netcfg::run(
        mac.try_into().map_err(|_| "MAC must have six octets")?, generation,
    )
}
