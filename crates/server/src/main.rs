#![forbid(unsafe_code)]

use std::net::TcpListener;

use clap::Parser;
use common::DbConfig;
use engine::Database;
use metrics_exporter_prometheus::PrometheusBuilder;

mod health;
mod http;
mod signals;

use health::Readiness;

#[derive(Parser, Debug)]
#[command(
    name = "simple_rdbms_server",
    about = "Headless simple_rdbms server: metrics and health endpoints"
)]
struct Args {
    #[arg(default_value = "simple_rdbms.db")]
    db_path: String,

    #[arg(long, default_value = "0.0.0.0:9090")]
    metrics_addr: String,
}

fn main() -> anyhow::Result<()> {
    init_logging();

    let args = Args::parse();
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
