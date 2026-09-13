use drv_dns_service::{Config, MAX_CLIENTS, forward::Forwarder, ingress, sandbox, trust};
use std::{
    io::{self, Read},
    sync::Arc,
};
fn run() -> io::Result<()> {
    // No tokio::main: establish confinement before runtime threads/input parsing.
    let capabilities = sandbox::lockdown()?;
    eprintln!("DNS_LOCKED");
    let mut config = String::new();
    capabilities.config.take(8193).read_to_string(&mut config)?;
    if config.len() > 8192 {
        return Err(io::Error::other("config too large"));
    }
    let config = Config::parse(&config)?;
    let mut pem = Vec::new();
    capabilities.ca.take(2097153).read_to_end(&mut pem)?;
    if pem.len() > 2097152 {
        return Err(io::Error::other("CA bundle too large"));
    }
    let tls = trust(&pem)?;
    drop(pem);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_io()
        .enable_time()
        .build()?;
    runtime.block_on(async {
        let engine = Arc::new(Forwarder::new(config, tls));
        let capacity = Arc::new(tokio::sync::Semaphore::new(MAX_CLIENTS));
        let mut tasks = tokio::task::JoinSet::new();
        for socket in capabilities.udp {
            tasks.spawn(ingress::udp(
                tokio::net::UdpSocket::from_std(socket)?,
                engine.clone(),
                capacity.clone(),
            ));
        }
        for listener in capabilities.tcp {
            tasks.spawn(ingress::tcp(
                tokio::net::TcpListener::from_std(listener)?,
                engine.clone(),
                capacity.clone(),
            ));
        }
        tasks.spawn(ingress::nss(
            tokio::net::UnixListener::from_std(capabilities.nss)?,
            engine,
            capacity,
        ));
        eprintln!("DNS_READY workers=2 upstream=encrypted-only");
        match tasks.join_next().await {
            Some(Ok(result)) => result,
            _ => Err(io::Error::other("DNS listener failed")),
        }
    })
}
fn main() {
    if let Err(error) = run() {
        eprintln!("DNS service failed: {error}");
        std::process::exit(1);
    }
}
