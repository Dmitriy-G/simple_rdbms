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
