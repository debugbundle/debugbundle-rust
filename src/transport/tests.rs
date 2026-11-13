//! Real TLS handshakes against a synthetic loopback server. Additional test
//! roots are private to this client; no machine or user trust store is modified.
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use rustls::{pki_types::PrivatePkcs8KeyDer, ServerConfig, ServerConnection, StreamOwned};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    thread::{self, JoinHandle},
};

fn bytes(name: &str) -> Vec<u8> {
    let fixture: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/tls-certificates.json")).unwrap();
    STANDARD.decode(fixture[name].as_str().unwrap()).unwrap()
}

fn server(leaf: &str, intermediate: bool, response: &'static str) -> (String, JoinHandle<Vec<u8>>) {
    let mut certificates = vec![bytes(leaf).into()];
    if intermediate {
        certificates.push(bytes("intermediate").into());
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            certificates,
            PrivatePkcs8KeyDer::from(bytes("server-key")).into(),
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        let socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return Vec::new(),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut tls = StreamOwned::new(ServerConnection::new(Arc::new(config)).unwrap(), socket);
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while request.len() < 16 * 1024 {
            match tls.read(&mut buffer) {
                Ok(0) | Err(_) => return request,
                Ok(size) => request.extend_from_slice(&buffer[..size]),
            }
            if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let _ = tls.write_all(response.as_bytes());
                let _ = tls.flush();
                return request;
            }
        }
        request
    });
    (format!("https://localhost:{port}/v1/events"), worker)
}

fn trusted(endpoint: &str) -> HttpTransport {
    let builder = reqwest::Client::builder()
        .tls_certs_merge([reqwest::Certificate::from_der(&bytes("root")).unwrap()]);
    HttpTransport::build(
        endpoint,
        "synthetic-tls-token",
        Duration::from_secs(1),
        false,
        None,
        builder,
    )
    .unwrap()
}

const ACCEPTED: &str = "HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nContent-Length: 39\r\nConnection: close\r\n\r\n{\"accepted\":1,\"rejected\":0,\"errors\":[]}";

#[tokio::test]
async fn verified_tls_chain_delivers_authenticated_event_and_config_requests() {
    for config_request in [false, true] {
        let (endpoint, worker) = server("server", true, ACCEPTED);
        let transport = trusted(&endpoint);
        let reply = if config_request {
            transport
                .config(Some("test-etag"), "checkout", "test")
                .await
        } else {
            transport.send(br#"{"events":[{}]}"#.to_vec()).await
        }
        .unwrap();
        assert_eq!(reply.status, 202);
        assert_eq!(reply.body["accepted"], 1);
        let request = String::from_utf8(worker.join().unwrap())
            .unwrap()
            .to_lowercase();
        assert!(request.contains("authorization: bearer synthetic-tls-token\r\n"));
        assert!(request.contains("x-debugbundle-evidence: 1\r\n"));
        assert!(request.starts_with(if config_request {
            "get /v1/sdk/config?"
        } else {
            "post /v1/events "
        }));
        if config_request {
            assert!(request.contains("if-none-match: test-etag\r\n"));
        }
    }
}

#[tokio::test]
async fn untrusted_tls_cannot_receive_project_credentials() {
    let (endpoint, worker) = server("server", true, ACCEPTED);
    let transport = HttpTransport::new(
        &endpoint,
        "synthetic-tls-token",
        Duration::from_secs(1),
        false,
        None,
    )
    .unwrap();
    assert!(transport.send(br#"{"events":[]}"#.to_vec()).await.is_err());
    assert!(worker.join().unwrap().is_empty());
}

#[tokio::test]
async fn tls_rejects_wrong_hostname_missing_intermediate_and_expired_leaf_before_http() {
    for (leaf, intermediate, wrong_host) in [
        ("server", true, true),
        ("server", false, false),
        ("expired", true, false),
    ] {
        let (endpoint, worker) = server(leaf, intermediate, ACCEPTED);
        let endpoint = if wrong_host {
            endpoint.replace("localhost", "127.0.0.1")
        } else {
            endpoint
        };
        assert!(trusted(&endpoint)
            .send(br#"{"events":[]}"#.to_vec())
            .await
            .is_err());
        assert!(
            worker.join().unwrap().is_empty(),
            "invalid TLS received HTTP bytes"
        );
    }
}

#[tokio::test]
async fn tls_redirects_never_forward_credentials() {
    let (endpoint, worker) = server("server", true, "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:9/forbidden\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
    let response = trusted(&endpoint)
        .send(br#"{"events":[]}"#.to_vec())
        .await
        .unwrap();
    assert_eq!(response.status, 307);
    assert!(!worker.join().unwrap().is_empty());
}

#[tokio::test]
async fn stalled_tls_handshake_is_bounded_by_transport_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!(
        "https://localhost:{}/v1/events",
        listener.local_addr().unwrap().port()
    );
    // The bound listener accepts no handshake: no detached or blocking fixture thread.
    let transport = HttpTransport::new(
        &endpoint,
        "synthetic",
        Duration::from_millis(50),
        false,
        None,
    )
    .unwrap();
    let started = Instant::now();
    assert!(transport.send(br#"{"events":[]}"#.to_vec()).await.is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}
