//! `simple_rdbms`: the REPL front end. Opens a database file and reads SQL
//! statements from stdin, one line at a time, executing each against the
//! open `Database` and printing its result or error.

#![forbid(unsafe_code)]

use std::io::{self, BufRead, Write};

use clap::Parser;
use common::DbConfig;
use engine::{Database, ResultSet};

/// Command-line arguments for the `simple_rdbms` REPL.
#[derive(Parser, Debug)]
#[command(name = "simple_rdbms", about = "A relational database engine, from scratch")]
struct Cli {
    /// Path to the database file to open, created if it does not already
    /// exist.
    #[arg(default_value = "simple_rdbms.db")]
    db_path: String,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let config = DbConfig::new(cli.db_path);
    let mut db = Database::open(config)?;

    run_repl(&mut db)?;

    db.close()?;
    Ok(())
}

/// Reads statements from stdin until EOF or an `exit`/`quit` line, printing
/// each statement's result or error before prompting for the next one.
fn run_repl(db: &mut Database) -> anyhow::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    prompt(&mut stdout)?;
    for line in stdin.lock().lines() {
        let line = line?;
        let statement = line.trim();

        if statement.is_empty() {
            prompt(&mut stdout)?;
            continue;
        }
        if statement.eq_ignore_ascii_case("exit") || statement.eq_ignore_ascii_case("quit") {
            break;
        }

        match db.execute(statement) {
            Ok(result_set) => print_result(&result_set),
            Err(err) => eprintln!("error: {err}"),
        }

        prompt(&mut stdout)?;
    }

    Ok(())
}

/// Writes the REPL's prompt and flushes it, since it is not followed by a
/// newline.
fn prompt(stdout: &mut impl Write) -> anyhow::Result<()> {
    write!(stdout, "simple_rdbms> ")?;
    stdout.flush()?;
    Ok(())
}

/// Prints a result set: a header of column names followed by one line per
/// row, or `OK` for statements with no output columns.
fn print_result(result_set: &ResultSet) {
    if result_set.columns().is_empty() {
        println!("OK");
        return;
    }

    println!("{}", result_set.columns().join(" | "));
    for row in result_set.rows() {
        let cells: Vec<String> = row.values().iter().map(|value| format!("{value:?}")).collect();
        println!("{}", cells.join(" | "));
    }
}
