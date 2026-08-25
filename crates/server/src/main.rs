#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use clap::Parser;
use common::DbConfig;
use engine::Database;
use metrics_exporter_prometheus::PrometheusBuilder;
use server::health::Readiness;
use server::{http, signals};

const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Parser, Debug)]
#[command(
    name = "simple_rdbms_server",
    about = "Headless simple_rdbms server: metrics and health endpoints"
)]
struct Args {
    #[arg(default_value = "simple_rdbms.db")]
    db_path: String,

    #[arg(long, default_value = "0.0.0.0:9090", env = "SIMPLE_RDBMS_METRICS_ADDR")]
    metrics_addr: String,

    #[arg(
        long,
        help = "Query this process's own /health/ready over HTTP, exit 0 if ready or 1 \
                otherwise, and do nothing else - for HEALTHCHECK, in place of curl"
    )]
    health_check: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    if args.health_check {
        std::process::exit(if check_ready(&args.metrics_addr) { 0 } else { 1 });
    }

    init_logging();

    let readiness = Readiness::new();

    let handle = PrometheusBuilder::new().install_recorder()?;
    let listener = TcpListener::bind(&args.metrics_addr)?;
    tracing::info!(addr = %args.metrics_addr, "metrics/health endpoint listening");

    let server_readiness = readiness.clone();
    let _http_handle = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            http::serve(listener, handle, server_readiness);
        }));
        match result {
            Ok(()) => tracing::error!(
                "metrics/health HTTP listener exited; metrics and health endpoints are no \
                 longer being served"
            ),
            Err(_) => tracing::error!(
                "metrics/health HTTP listener panicked; metrics and health endpoints are no \
                 longer being served"
            ),
        }
    });

    let config = DbConfig::new(&args.db_path);
    let db = Database::open(config)?;
    readiness.set_ready();
    tracing::info!("ready to accept statements");

    signals::wait_for_shutdown_signal()?;

    readiness.set_not_ready();
    tracing::info!("shutdown signal received, checkpointing and closing");
    db.close()?;
    Ok(())
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().json().with_env_filter(filter).init();
}

fn check_ready(metrics_addr: &str) -> bool {
    let port = metrics_addr.rsplit(':').next().unwrap_or("9090");
    let target = format!("127.0.0.1:{port}");

    let mut stream = match TcpStream::connect(&target) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("health check: could not connect to {target}: {err}");
            return false;
        }
    };
    let _ = stream.set_read_timeout(Some(HEALTH_CHECK_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HEALTH_CHECK_TIMEOUT));

    let request = "GET /health/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    if let Err(err) = stream.write_all(request.as_bytes()) {
        eprintln!("health check: failed to send request to {target}: {err}");
        return false;
    }

    let mut response = String::new();
    if let Err(err) = stream.read_to_string(&mut response) {
        eprintln!("health check: failed to read response from {target}: {err}");
        return false;
    }

    let status_line = response.lines().next().unwrap_or("<empty response>");
    let ready = status_line.contains("200");
    if !ready {
        eprintln!("health check: {target} reported not ready: {status_line}");
    }
    ready
}
