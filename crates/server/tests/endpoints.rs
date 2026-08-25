use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};

use metrics_exporter_prometheus::PrometheusBuilder;
use server::health::Readiness;
use server::http::serve;

#[cfg(test)]
fn spawn_test_server(readiness: Readiness) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("read local addr");
    let handle = PrometheusBuilder::new().build_recorder().handle();
    std::thread::spawn(move || serve(listener, handle, readiness));
    addr
}

#[cfg(test)]
fn get(addr: SocketAddr, path: &str) -> (String, String) {
    let mut stream = TcpStream::connect(addr).expect("connect to test server");
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").expect("write request line");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    let (head, body) = response.split_once("\r\n\r\n").expect("response has a body");
    let status = head.lines().next().expect("response has a status line").to_string();
    (status, body.to_string())
}

#[test]
fn health_ready_reports_each_non_ready_state_as_503_and_names_it() {
    let readiness = Readiness::new();
    let addr = spawn_test_server(readiness.clone());

    let (status, body) = get(addr, "/health/ready");
    assert!(status.contains("503"), "status: {status}");
    assert_eq!(body, "starting\n");

    readiness.set_ready();
    let (status, body) = get(addr, "/health/ready");
    assert!(status.contains("200"), "status: {status}");
    assert_eq!(body, "ready\n");

    readiness.set_not_ready();
    let (status, body) = get(addr, "/health/ready");
    assert!(status.contains("503"), "status: {status}");
    assert_eq!(body, "shutting_down\n");
}
