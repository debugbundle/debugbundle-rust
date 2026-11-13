#![cfg(feature = "http")]
use debugbundle::transport::HttpTransport;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    time::{Duration, Instant},
};

fn server(
    status: &'static str,
    body: Vec<u8>,
    delay: Duration,
) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut buffer = vec![0; 16384];
        let n = stream.read(&mut buffer).unwrap();
        let _ = tx.send(String::from_utf8_lossy(&buffer[..n]).into_owned());
        std::thread::sleep(delay);
        let header=format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(&body);
    });
    (format!("http://{address}/v1/events"), rx)
}

#[tokio::test]
async fn native_transport_authenticates_and_requires_a_bounded_json_response() {
    let (endpoint, request) = server(
        "202 Accepted",
        br#"{"accepted":1,"rejected":0,"errors":[]}"#.to_vec(),
        Duration::ZERO,
    );
    let transport = HttpTransport::new(
        &endpoint,
        "synthetic-project-token",
        Duration::from_secs(1),
        true,
        None,
    )
    .unwrap();
    let reply = transport
        .send(br#"{"events":[{}]}"#.to_vec())
        .await
        .unwrap();
    assert_eq!(reply.status, 202);
    assert_eq!(reply.body["accepted"], 1);
    let request = request
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .to_lowercase();
    assert!(request.contains("authorization: bearer synthetic-project-token"));
    assert!(request.contains("x-debugbundle-evidence: 1"));
}

#[tokio::test]
async fn oversized_and_stalled_responses_fail_without_unbounded_reads() {
    let (endpoint, _) = server("202 Accepted", vec![b' '; 262145], Duration::ZERO);
    let transport =
        HttpTransport::new(&endpoint, "synthetic", Duration::from_secs(1), true, None).unwrap();
    assert!(transport.send(br#"{"events":[]}"#.to_vec()).await.is_err());
    let (endpoint, _) = server("202 Accepted", b"{}".to_vec(), Duration::from_millis(500));
    let transport = HttpTransport::new(
        &endpoint,
        "synthetic",
        Duration::from_millis(50),
        true,
        None,
    )
    .unwrap();
    let started = Instant::now();
    assert!(transport.send(br#"{"events":[]}"#.to_vec()).await.is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn insecure_endpoints_and_url_credentials_are_not_production_defaults() {
    for endpoint in [
        "http://example.test/v1/events",
        "https://user:secret@example.test/v1/events",
        "file:///tmp/events",
        "https://example.test/#secret",
    ] {
        assert!(
            HttpTransport::new(endpoint, "synthetic", Duration::from_secs(1), false, None)
                .is_none()
        );
    }
}

#[tokio::test]
async fn low_level_transport_rejects_credential_queries_and_discovery_identity() {
    assert!(HttpTransport::new(
        "https://api.example.test/v1/events?token=SYNTHETIC_SECRET",
        "synthetic",
        Duration::from_secs(1),
        false,
        None
    )
    .is_none());
    let transport = HttpTransport::new(
        "https://api.example.test/v1/events?mode=a%20b",
        "synthetic",
        Duration::from_secs(1),
        false,
        None,
    )
    .unwrap();
    assert!(transport
        .config(None, "dbundle_proj_SYNTHETIC_SECRET", "test")
        .await
        .is_err());
}
