#![forbid(unsafe_code)]

use clap::Parser;
use cli::run_repl;
use common::DbConfig;
use engine::Database;

#[derive(Parser, Debug)]
#[command(name = "simple_rdbms", about = "A relational database engine, from scratch")]
struct Cli {
    #[arg(default_value = "simple_rdbms.db")]
    db_path: String,
}

fn main() -> anyhow::Result<()> {
    init_logging();

    let cli = Cli::parse();
    let config = DbConfig::new(cli.db_path);
    let mut db = Database::open(config)?;

    run_repl(&mut db)?;

    db.close()?;
    Ok(())
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().json().with_writer(std::io::stderr).with_env_filter(filter).init();
}
