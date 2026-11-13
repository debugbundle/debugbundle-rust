//! A real loopback HTTP peer holds the ingestion response after reading its body.
//! Server waits and initialization are outside every caller measurement.
use crate::{capture, support::Gate};
use debugbundle::{Client, Config};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

pub fn pressure() -> Vec<Value> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1/events", listener.local_addr().unwrap());
    let (gate, started) = Gate::new();
    let server_gate = gate.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let server = std::thread::spawn(move || {
        while !stopping.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut socket, _)) => serve(&mut socket, &server_gate),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(error) => panic!("Loopback benchmark listener failed: {error}"),
            }
        }
    });
    let client = Client::new(Config {
        endpoint,
        allow_insecure_http: true,
        batch_size: 1,
        max_buffered_events: 1,
        flush_interval: Duration::from_secs(300),
        request_timeout: Duration::from_secs(10),
        evidence_capabilities: Some(json!({"version":1,"features":debugbundle::wire::FEATURES})),
        ..Config::new("synthetic-native-peer", "resource-native-peer")
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    while !client.remote_config_ready() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let result = capture::pressure_calls("native-http", client, gate, started);
    stop.store(true, Ordering::Release);
    server.join().unwrap();
    result
}

fn serve(socket: &mut TcpStream, gate: &Gate) {
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        socket.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
        assert!(head.len() <= 16384);
    }
    let head = String::from_utf8(head).unwrap().to_ascii_lowercase();
    assert!(head.contains("authorization: bearer synthetic-native-peer"));
    let body = if head.starts_with("get ") {
        json!({"tier":"free","probes":{"enabled":false},"evidence_capabilities":{"version":1,"features":debugbundle::wire::FEATURES}})
    } else {
        assert!(head.starts_with("post /v1/events "));
        let length: usize = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(length <= debugbundle::wire::MAX_BODY_BYTES);
        let mut body = vec![0; length];
        socket.read_exact(&mut body).unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let count = body["events"].as_array().unwrap().len();
        gate.wait();
        json!({"accepted":count,"rejected":0,"errors":[]})
    };
    let body = serde_json::to_vec(&body).unwrap();
    write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
    socket.write_all(&body).unwrap();
}
