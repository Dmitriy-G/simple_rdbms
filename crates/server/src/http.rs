use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

use metrics_exporter_prometheus::PrometheusHandle;

use crate::health::{Readiness, ReadinessState};

pub fn serve(listener: TcpListener, handle: PrometheusHandle, readiness: Readiness) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => handle_connection(stream, &handle, &readiness),
            Err(err) => tracing::warn!(%err, "metrics listener accept failed"),
        }
    }
}

fn handle_connection(mut stream: TcpStream, handle: &PrometheusHandle, readiness: &Readiness) {
    let Some(path) = read_request_path(&stream) else {
        return;
    };

    let (status, body) = match path.as_str() {
        "/metrics" => ("200 OK", handle.render()),
        "/health/live" => ("200 OK", "live\n".to_string()),
        "/health/ready" => match readiness.state() {
            ReadinessState::Ready => ("200 OK", "ready\n".to_string()),
            other => ("503 Service Unavailable", format!("{other}\n")),
        },
        _ => ("404 Not Found", "not found\n".to_string()),
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: \
         {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn read_request_path(stream: &TcpStream) -> Option<String> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let path = request_line.split_whitespace().nth(1)?;
    Some(path.to_string())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::io::Read;
    use std::net::SocketAddr;

    use metrics_exporter_prometheus::PrometheusBuilder;

    use super::*;

    fn spawn_test_server(readiness: Readiness) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("read local addr");
        let handle = PrometheusBuilder::new().build_recorder().handle();
        std::thread::spawn(move || serve(listener, handle, readiness));
        addr
    }

    fn get(addr: SocketAddr, path: &str) -> (String, String) {
        let mut stream = TcpStream::connect(addr).expect("connect to test server");
        write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("write request line");
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
}
