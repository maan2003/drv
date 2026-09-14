//! Fixed-size, versioned local resolver protocol. No pointers or native structs on the wire.
#![forbid(unsafe_code)]
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
pub const PATH: &str = "/run/drv-resolver.sock";
pub const REQUEST_LEN: usize = 264;
pub const MAX_ADDRESSES: usize = 64;
pub const RESPONSE_LEN: usize = 8 + MAX_ADDRESSES * 17;
pub const OK: u8 = 0;
pub const NOT_FOUND: u8 = 1;
pub const TEMPORARY: u8 = 2;
pub const UNAVAILABLE: u8 = 3;

pub fn request(name: &str) -> Option<[u8; REQUEST_LEN]> {
    if name.is_empty() || name.len() > 254 || name.as_bytes().contains(&0) {
        return None;
    }
    let mut out = [0; REQUEST_LEN];
    out[..4].copy_from_slice(b"DRD1");
    out[4..6].copy_from_slice(&(name.len() as u16).to_le_bytes());
    out[8..8 + name.len()].copy_from_slice(name.as_bytes());
    Some(out)
}
pub fn name(input: &[u8; REQUEST_LEN]) -> Option<&str> {
    let len = u16::from_le_bytes(input[4..6].try_into().ok()?) as usize;
    if &input[..4] != b"DRD1"
        || len == 0
        || len > 254
        || input[6..8] != [0; 2]
        || input[8 + len..].iter().any(|b| *b != 0)
    {
        return None;
    }
    let name = std::str::from_utf8(&input[8..8 + len]).ok()?;
    if name.as_bytes().contains(&0) {
        return None;
    }
    Some(name)
}
pub fn response(status: u8, addresses: &[IpAddr]) -> [u8; RESPONSE_LEN] {
    let mut out = [0; RESPONSE_LEN];
    out[..4].copy_from_slice(b"DRD1");
    out[4] = status;
    if status != OK {
        return out;
    }
    out[5] = addresses.len().min(MAX_ADDRESSES) as u8;
    for (ip, slot) in addresses.iter().zip(out[8..].chunks_exact_mut(17)) {
        match ip {
            IpAddr::V4(ip) => {
                slot[0] = 4;
                slot[1..5].copy_from_slice(&ip.octets());
            }
            IpAddr::V6(ip) => {
                slot[0] = 6;
                slot[1..].copy_from_slice(&ip.octets());
            }
        }
    }
    out
}
pub fn addresses(input: &[u8; RESPONSE_LEN]) -> Option<Result<Vec<IpAddr>, u8>> {
    let count = input[5] as usize;
    if &input[..4] != b"DRD1"
        || input[4] > UNAVAILABLE
        || count > MAX_ADDRESSES
        || input[6..8] != [0; 2]
        || input[8 + count * 17..].iter().any(|b| *b != 0)
    {
        return None;
    }
    if input[4] != OK {
        return (count == 0).then_some(Err(input[4]));
    }
    let mut result = Vec::with_capacity(count);
    for slot in input[8..8 + count * 17].chunks_exact(17) {
        result.push(match slot[0] {
            4 if slot[5..].iter().all(|b| *b == 0) => {
                IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(&slot[1..5]).ok()?))
            }
            6 => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(&slot[1..]).ok()?)),
            _ => return None,
        });
    }
    Some(Ok(result))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_and_reject_malformed() {
        assert_eq!(
            name(&request("example.test.").unwrap()),
            Some("example.test.")
        );
        assert!(request("").is_none());
        assert!(request(&"a".repeat(255)).is_none());
        let ips = ["192.0.2.1".parse().unwrap(), "::1".parse().unwrap()];
        let mut bytes = response(OK, &ips);
        assert_eq!(addresses(&bytes), Some(Ok(ips.to_vec())));
        bytes[8] = 9;
        assert!(addresses(&bytes).is_none());
        let mut bytes = request("example.test").unwrap();
        bytes[7] = 1;
        assert!(name(&bytes).is_none());
        assert_eq!(addresses(&response(NOT_FOUND, &[])), Some(Err(NOT_FOUND)));
    }
}
