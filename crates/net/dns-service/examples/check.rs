use drv_dns_service::{MAX_MESSAGE, forward::encode};
use hickory_proto::{
    op::{Message, Query, ResponseCode},
    rr::{Name, RecordType},
};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream, UdpSocket},
    time::Duration,
};
fn lookup(server: SocketAddr, tcp: bool, name: &str, kind: RecordType) -> Message {
    let mut q = Message::query();
    q.metadata.id = 0x3412;
    q.metadata.recursion_desired = true;
    q.add_query(Query::query(Name::from_ascii(name).unwrap(), kind));
    let bytes = encode(&q, MAX_MESSAGE).unwrap();
    let result = if tcp {
        let mut s = TcpStream::connect_timeout(&server, Duration::from_secs(5)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(&(bytes.len() as u16).to_be_bytes()).unwrap();
        s.write_all(&bytes).unwrap();
        let mut len = [0; 2];
        s.read_exact(&mut len).unwrap();
        let mut result = vec![0; u16::from_be_bytes(len) as usize];
        s.read_exact(&mut result).unwrap();
        result
    } else {
        let s = UdpSocket::bind(if server.is_ipv4() {
            "127.0.0.1:0"
        } else {
            "[::1]:0"
        })
        .unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.connect(server).unwrap();
        s.send(&bytes).unwrap();
        let mut result = vec![0; MAX_MESSAGE];
        let n = s.recv(&mut result).unwrap();
        result.truncate(n);
        result
    };
    let a = Message::from_vec(&result).unwrap();
    assert_eq!(a.id, q.id);
    assert_eq!(a.queries, q.queries);
    a
}
fn main() {
    for server in ["127.0.0.1:53", "[::1]:53"] {
        for tcp in [false, true] {
            for kind in [
                RecordType::A,
                RecordType::AAAA,
                RecordType::MX,
                RecordType::TXT,
            ] {
                let a = lookup(server.parse().unwrap(), tcp, "example.com.", kind);
                assert_eq!(
                    a.response_code,
                    ResponseCode::NoError,
                    "{server} tcp={tcp} {kind:?}"
                );
                assert!(!a.answers.is_empty(), "{kind:?}");
            }
            let a = lookup(
                server.parse().unwrap(),
                tcp,
                "cloudflare.com.",
                RecordType::HTTPS,
            );
            assert_eq!(a.response_code, ResponseCode::NoError);
            assert!(!a.answers.is_empty(), "HTTPS record");
            let a = lookup(
                server.parse().unwrap(),
                tcp,
                "drv-dns-missing.invalid.",
                RecordType::A,
            );
            assert_eq!(a.response_code, ResponseCode::NXDomain);
            assert!(!a.authorities.is_empty(), "negative SOA");
            println!("PASS_DNS server={server} tcp={tcp} A_AAAA_MX_TXT_HTTPS_NXDOMAIN_SOA");
        }
    }
    let threads: Vec<_> = (0..32)
        .map(|_| {
            std::thread::spawn(|| {
                let a = lookup(
                    "127.0.0.1:53".parse().unwrap(),
                    true,
                    "example.com.",
                    RecordType::A,
                );
                assert_eq!(a.response_code, ResponseCode::NoError);
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    println!("PASS_DNS_CONCURRENT_32");
    let mut s = std::os::unix::net::UnixStream::connect(drv_dns_wire::PATH).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(&drv_dns_wire::request("example.com.").unwrap())
        .unwrap();
    let mut out = [0; drv_dns_wire::RESPONSE_LEN];
    s.read_exact(&mut out).unwrap();
    let ips = drv_dns_wire::addresses(&out).unwrap().unwrap();
    assert!(ips.iter().any(|a| a.is_ipv4()));
    assert!(ips.iter().any(|a| a.is_ipv6()));
    use std::net::ToSocketAddrs;
    assert!(
        ("example.com", 443)
            .to_socket_addrs()
            .unwrap()
            .next()
            .is_some()
    );
    println!("PASS_DNS_NSS_AND_GLIBC");
}
