mod cache;
pub mod forward;
pub mod ingress;
pub mod sandbox;
mod transport;

use serde::Deserialize;
use std::{io, net::SocketAddr};

pub const MAX_MESSAGE: usize = 65535;
pub const MAX_CLIENTS: usize = 64;
pub const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upstream {
    pub address: SocketAddr,
    pub server_name: String,
    pub path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub upstreams: Vec<Upstream>,
}
impl Config {
    pub fn parse(input: &str) -> io::Result<Self> {
        let config: Self = toml::from_str(input).map_err(io::Error::other)?;
        if !(1..=4).contains(&config.upstreams.len()) {
            return Err(io::Error::other("expected 1..4 encrypted upstreams"));
        }
        for u in &config.upstreams {
            if u.address.port() == 0
                || u.address.ip().is_unspecified()
                || u.server_name.len() > 253
                || u.path.len() > 1024
                || !u.path.starts_with('/')
                || u.path.starts_with("//")
                || u.path.parse::<http::uri::PathAndQuery>().is_err()
                || rustls::pki_types::ServerName::try_from(u.server_name.clone()).is_err()
                || u.server_name.parse::<std::net::IpAddr>().is_ok()
            {
                return Err(io::Error::other("invalid encrypted upstream"));
            }
        }
        Ok(config)
    }
}
pub fn trust(pem: &[u8]) -> io::Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut &pem[..]) {
        roots.add(cert?).map_err(io::Error::other)?;
    }
    if roots.is_empty() {
        return Err(io::Error::other("empty trust roots"));
    }
    let mut tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(io::Error::other)?
    .with_root_certificates(roots)
    .with_no_client_auth();
    tls.alpn_protocols = vec![b"h2".to_vec()];
    tls.resumption = rustls::client::Resumption::disabled();
    Ok(tls)
}
