//! `simple_rdbms`: the REPL front end. Opens a database file and reads SQL
//! statements from stdin, buffering lines until a `;` terminates a
//! statement, executing each against the open `Database` and printing its
//! result or error.

#![forbid(unsafe_code)]

use std::io::{self, BufRead, Write};

use clap::Parser;
use common::DbConfig;
use engine::{DataType, Database, ResultSet, Tuple, Value};

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

/// Reads statements from stdin, buffering lines until a `;` terminates one,
/// printing that statement's result or error before prompting for the next.
/// A leading `.`-command (`.tables`, `.schema <name>`, `.exit`) is handled
/// immediately instead, without needing a `;`. A query error prints and
/// returns to the prompt; it never ends the REPL.
fn run_repl(db: &mut Database) -> anyhow::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut buffer = String::new();

    prompt(&mut stdout, &buffer)?;
    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();

        if buffer.is_empty() {
            if trimmed.is_empty() {
                prompt(&mut stdout, &buffer)?;
                continue;
            }
            if trimmed.eq_ignore_ascii_case(".exit") {
                break;
            }
            if trimmed.eq_ignore_ascii_case(".tables") {
                print_tables(db);
                prompt(&mut stdout, &buffer)?;
                continue;
            }
            if let Some(name) = trimmed.strip_prefix(".schema ") {
                print_schema(db, name.trim());
                prompt(&mut stdout, &buffer)?;
                continue;
            }
        }

        buffer.push_str(&line);
        buffer.push('\n');

        if trimmed.ends_with(';') {
            let statement = buffer.trim().trim_end_matches(';').trim();
            match db.execute(statement) {
                Ok(result_set) => print_result(&result_set),
                Err(err) => eprintln!("error: {err}"),
            }
            buffer.clear();
        }
        prompt(&mut stdout, &buffer)?;
    }

    Ok(())
}

/// Writes the REPL's prompt and flushes it, since it is not followed by a
/// newline. Mid-statement (a `;` not yet seen), a continuation prompt marks
/// that more input is expected.
fn prompt(stdout: &mut impl Write, buffer: &str) -> anyhow::Result<()> {
    if buffer.is_empty() {
        write!(stdout, "simple_rdbms> ")?;
    } else {
        write!(stdout, "        ...> ")?;
    }
    stdout.flush()?;
    Ok(())
}

/// `.tables`: lists every table in the database, one per line.
fn print_tables(db: &Database) {
    for name in db.table_names() {
        println!("{name}");
    }
}

/// `.schema <name>`: prints one line per column as `name TYPE [NOT NULL]`.
fn print_schema(db: &Database, name: &str) {
    match db.table_schema(name) {
        Ok(schema) => {
            for column in schema.columns() {
                let suffix = if column.nullable { "" } else { " NOT NULL" };
                println!("{} {}{suffix}", column.name, format_data_type(column.data_type));
            }
        }
        Err(err) => eprintln!("error: {err}"),
    }
}

/// Renders a `DataType` the way it was spelled in `CREATE TABLE`, e.g. `TEXT`
/// rather than the parser's internal `Varchar(4294967295)` for it.
fn format_data_type(data_type: DataType) -> String {
    match data_type {
        DataType::Boolean => "BOOLEAN".to_string(),
        DataType::Integer => "INTEGER".to_string(),
        DataType::BigInt => "BIGINT".to_string(),
        DataType::Double => "DOUBLE".to_string(),
        DataType::Varchar(u32::MAX) => "TEXT".to_string(),
        DataType::Varchar(len) => format!("VARCHAR({len})"),
    }
}

/// Prints a result set: an aligned text table for a query's rows, `NULL` for
/// SQL `NULL`, or a rows-affected line for a statement with no output rows.
fn print_result(result_set: &ResultSet) {
    match result_set {
        ResultSet::Rows { columns, rows } => print_table(columns, rows),
        ResultSet::RowsAffected(0) => println!("OK"),
        ResultSet::RowsAffected(count) => {
            println!("OK ({count} row{})", if *count == 1 { "" } else { "s" })
        }
    }
}

/// Prints `columns` and `rows` as a text table whose columns are padded to
/// the widest cell (header included) in that column.
fn print_table(columns: &[String], rows: &[Tuple]) {
    let formatted_rows: Vec<Vec<String>> =
        rows.iter().map(|row| row.values().iter().map(format_value).collect()).collect();

    let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
    for row in &formatted_rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.len());
        }
    }

    let header: Vec<String> = columns.iter().zip(&widths).map(|(c, w)| format!("{c:w$}")).collect();
    println!("{}", header.join(" | "));
    println!("{}", widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>().join("-+-"));
    for row in &formatted_rows {
        let cells: Vec<String> = row.iter().zip(&widths).map(|(c, w)| format!("{c:w$}")).collect();
        println!("{}", cells.join(" | "));
    }
}

/// Renders one cell's value, `NULL` as the literal text `NULL` per spec.
fn format_value(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(v) => v.to_string(),
        Value::BigInt(v) => v.to_string(),
        Value::Double(v) => v.to_string(),
        Value::Varchar(s) => s.clone(),
    }
}
