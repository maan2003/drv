use bytes::Bytes;
use drv_dns_service::{
    Config,
    forward::{Forwarder, encode},
    trust,
};
use hickory_proto::{
    op::{Message, Query, ResponseCode},
    rr::{
        Name, RData, Record, RecordType,
        rdata::{A, SOA, TXT},
    },
};
use std::{sync::Arc, time::Duration};
use tokio::{net::TcpListener, task::JoinHandle, time::Instant};
use tokio_rustls::TlsAcceptor;

fn query(kind: RecordType) -> Message {
    let mut q = Message::query();
    q.metadata.id = 4321;
    q.metadata.recursion_desired = true;
    q.add_query(Query::query(
        Name::from_ascii("example.test.").unwrap(),
        kind,
    ));
    q
}
fn answer(q: &Message) -> Message {
    let mut a = Message::response(q.id, q.op_code);
    a.metadata.recursion_desired = q.recursion_desired;
    a.metadata.recursion_available = true;
    a.queries = q.queries.clone();
    a.answers.push(Record::from_rdata(
        q.queries[0].name().clone(),
        60,
        RData::A(A("192.0.2.8".parse().unwrap())),
    ));
    a
}
async fn server(mode: &'static str) -> (Forwarder, JoinHandle<()>) {
    let certified = rcgen::generate_simple_self_signed(vec!["resolver.test".to_string()]).unwrap();
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![certified.cert.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der()).into(),
    )
    .unwrap();
    let mut tls = tls;
    tls.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = Config::parse(&format!(
        "[[upstreams]]\naddress='{address}'\nserver_name='{}'\npath='/dns-query'",
        if mode == "bad-tls" {
            "wrong.test"
        } else {
            "resolver.test"
        }
    ))
    .unwrap();
    let engine = Forwarder::new(config, trust(certified.cert.pem().as_bytes()).unwrap());
    let task = tokio::spawn(async move {
        let mut peers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = peers.join_next(), if !peers.is_empty() => {}
                result = listener.accept() => {
                    let (tcp, _) = result.unwrap();
                    let acceptor = acceptor.clone();
                    peers.spawn(async move {
                        let Ok(tls) = acceptor.accept(tcp).await else { return; };
                        let Ok(mut h2) = h2::server::handshake(tls).await else { return; };
                        let mut requests = tokio::task::JoinSet::new();
                        while let Some(Ok((request, mut sender))) = h2.accept().await {
                            requests.spawn(async move {
                                let mut body=request.into_body();
                                let mut bytes=Vec::new();
                                while let Some(Ok(chunk))=body.data().await {
                                    bytes.extend_from_slice(&chunk);
                                    body.flow_control().release_capacity(chunk.len()).unwrap();
                                }
                                let q=Message::from_vec(&bytes).unwrap();
                                if mode=="mixed" && q.queries[0].query_type()==RecordType::TXT {
                                    tokio::time::sleep(Duration::from_millis(800)).await;
                                } else if mode=="mixed" {
                                    tokio::time::sleep(Duration::from_millis(200)).await;
                                }
                                let mut a=answer(&q);
                                match mode {
                                    "nxdomain" | "negative-age" => {
                                        a.answers.clear(); a.metadata.response_code=ResponseCode::NXDomain;
                                        a.authorities.push(Record::from_rdata(Name::from_ascii("test.").unwrap(),30,
                                            RData::SOA(SOA::new(Name::from_ascii("ns.test.").unwrap(),Name::from_ascii("host.test.").unwrap(),1,60,60,60,20))));
                                    }
                                    "large" => {
                                        a.answers.clear();
                                        for _ in 0..30 {
                                            a.answers.push(Record::from_rdata(q.queries[0].name().clone(),60,
                                                RData::TXT(TXT::new(vec!["x".repeat(200)]))));
                                        }
                                    }
                                    "mismatch" => { a.queries[0]=Query::query(Name::from_ascii("other.test.").unwrap(),RecordType::A); }
                                    "slow" => { tokio::time::sleep(Duration::from_secs(10)).await; }
                                    _ => {}
                                }
                                let mut response=http::Response::builder().header("content-type","application/dns-message");
                                if mode=="age" || mode=="negative-age" { response=response.header("age","12"); }
                                if mode=="bad-type" { response=http::Response::builder().header("content-type","text/plain"); }
                                if mode=="http-error" { response=response.status(503); }
                                if mode=="declared-overflow" { response=response.header("content-length","999999"); }
                                let mut stream=sender.send_response(response.body(()).unwrap(),false).unwrap();
                                let bytes=if mode=="overflow" { vec![0;65536] } else { a.to_vec().unwrap() };
                                let _=stream.send_data(Bytes::from(bytes),true);
                            });
                        }
                    });
                }
            }
        }
    });
    (engine, task)
}
#[tokio::test]
async fn full_response_and_age() {
    for mode in ["normal", "nxdomain", "negative-age", "age"] {
        let (engine, server) = server(mode).await;
        let response = engine
            .exchange(
                query(RecordType::A),
                Instant::now() + Duration::from_secs(2),
            )
            .await;
        assert_eq!(response.id, 4321);
        if mode == "nxdomain" || mode == "negative-age" {
            assert_eq!(response.response_code, ResponseCode::NXDomain);
            assert_eq!(response.authorities.len(), 1);
            assert_eq!(
                response.authorities[0].ttl,
                if mode == "negative-age" { 8 } else { 20 }
            );
        } else {
            assert_eq!(response.response_code, ResponseCode::NoError);
            assert_eq!(response.answers[0].ttl, if mode == "age" { 48 } else { 60 });
        }
        server.abort();
    }
}
#[tokio::test]
async fn fail_closed() {
    for mode in [
        "bad-tls",
        "bad-type",
        "mismatch",
        "overflow",
        "declared-overflow",
        "http-error",
        "slow",
    ] {
        let (engine, server) = server(mode).await;
        let response = engine
            .exchange(
                query(RecordType::A),
                Instant::now() + Duration::from_millis(300),
            )
            .await;
        assert_eq!(response.response_code, ResponseCode::ServFail, "{mode}");
        server.abort();
    }
}
#[tokio::test]
async fn truncate_without_corrupting_message() {
    let (engine, server) = server("large").await;
    let response = engine
        .exchange(
            query(RecordType::TXT),
            Instant::now() + Duration::from_secs(2),
        )
        .await;
    assert_eq!(response.answers.len(), 30);
    let udp = encode(&response, 512).unwrap();
    assert!(udp.len() <= 512);
    assert!(Message::from_vec(&udp).unwrap().truncation);
    assert!(
        !Message::from_vec(&encode(&response, 65535).unwrap())
            .unwrap()
            .truncation
    );
    server.abort();
}
#[test]
fn reject_unknown_configuration() {
    assert!(Config::parse("upstreams=[]").is_err());
    assert!(Config::parse("bootstrap_resolvers=['8.8.8.8']\nupstreams=[]").is_err());
}

#[tokio::test]
async fn canceled_stream_does_not_abort_active_connection_owner() {
    let (engine, server) = server("mixed").await;
    // First establish and reuse one connection.
    let mut warm = query(RecordType::A);
    warm.queries[0] = Query::query(Name::from_ascii("warm.test.").unwrap(), RecordType::A);
    assert_eq!(
        engine
            .exchange(warm, Instant::now() + Duration::from_secs(2))
            .await
            .response_code,
        ResponseCode::NoError
    );
    let (slow, fast) = tokio::join!(
        engine.exchange(
            query(RecordType::TXT),
            Instant::now() + Duration::from_millis(100)
        ),
        engine.exchange(
            query(RecordType::A),
            Instant::now() + Duration::from_secs(2)
        )
    );
    assert_eq!(slow.response_code, ResponseCode::ServFail);
    assert_eq!(fast.response_code, ResponseCode::NoError);
    tokio::time::sleep(Duration::from_millis(1050)).await;
    let mut fresh = query(RecordType::A);
    fresh.queries[0] = Query::query(Name::from_ascii("fresh.test.").unwrap(), RecordType::A);
    assert_eq!(
        engine
            .exchange(fresh, Instant::now() + Duration::from_secs(2))
            .await
            .response_code,
        ResponseCode::NoError
    );
    server.abort();
}
#[test]
fn nss_does_not_accept_unrelated_or_wrong_family_answers() {
    use hickory_proto::rr::rdata::CNAME;
    let q = query(RecordType::A);
    let mut response = answer(&q);
    response.answers[0].name = Name::from_ascii("unrelated.test.").unwrap();
    assert!(
        drv_dns_service::ingress::addresses(q.queries[0].name(), RecordType::A, &response)
            .is_empty()
    );
    response.answers.push(Record::from_rdata(
        q.queries[0].name().clone(),
        60,
        RData::CNAME(CNAME(Name::from_ascii("unrelated.test.").unwrap())),
    ));
    assert_eq!(
        drv_dns_service::ingress::addresses(q.queries[0].name(), RecordType::A, &response).len(),
        1
    );
    assert!(
        drv_dns_service::ingress::addresses(q.queries[0].name(), RecordType::AAAA, &response)
            .is_empty()
    );
}

#[tokio::test]
async fn cache_reuses_full_response_and_separates_client_flags() {
    let (engine, server) = server("large").await;
    let q = query(RecordType::TXT);
    let response = engine
        .exchange(q.clone(), Instant::now() + Duration::from_secs(2))
        .await;
    assert_eq!(response.answers.len(), 30);
    assert!(
        Message::from_vec(&encode(&response, 512).unwrap())
            .unwrap()
            .truncation
    );
    // An expired upstream deadline proves this is a cache hit, not a new request.
    let mut other_id = q.clone();
    other_id.metadata.id = 99;
    let cached = engine.exchange(other_id, Instant::now()).await;
    assert_eq!(cached.id, 99);
    assert_eq!(cached.answers.len(), 30);
    assert!(!cached.truncation);
    let mut cd = q.clone();
    cd.metadata.checking_disabled = true;
    assert_eq!(
        engine.exchange(cd, Instant::now()).await.response_code,
        ResponseCode::ServFail
    );
    let mut edns = hickory_proto::op::Edns::new();
    edns.flags_mut().dnssec_ok = true;
    let mut dnssec = q;
    dnssec.set_edns(edns);
    assert_eq!(
        engine.exchange(dnssec, Instant::now()).await.response_code,
        ResponseCode::ServFail
    );
    server.abort();
}
#[tokio::test]
async fn unsupported_requests_fail_without_network() {
    let (engine, server) = server("normal").await;
    for kind in [RecordType::AXFR, RecordType::IXFR] {
        assert_eq!(
            engine
                .exchange(query(kind), Instant::now())
                .await
                .response_code,
            ResponseCode::Refused
        );
    }
    let mut q = query(RecordType::A);
    q.queries.clear();
    assert_eq!(
        engine.exchange(q, Instant::now()).await.response_code,
        ResponseCode::FormErr
    );
    let mut q = query(RecordType::A);
    let mut edns = hickory_proto::op::Edns::new();
    edns.set_version(1);
    q.set_edns(edns);
    let reply = engine.exchange(q, Instant::now()).await;
    let wire = encode(&reply, 512).unwrap();
    assert_eq!(wire[3] & 15, 0);
    let parsed = Message::from_vec(&wire).unwrap();
    assert_eq!(u16::from(parsed.response_code), 16);
    assert_eq!(parsed.edns.as_ref().unwrap().version(), 0);
    assert_eq!(parsed.edns.as_ref().unwrap().rcode_high(), 1);
    server.abort();
}
