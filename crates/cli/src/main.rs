#![forbid(unsafe_code)]

use std::io::{self, BufRead, Write};

use clap::Parser;
use common::DbConfig;
use engine::{DataType, Database, ResultSet, Tuple, Value};

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
            let statement = statement_from_buffer(&buffer);
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

fn statement_from_buffer(buffer: &str) -> &str {
    let trimmed = buffer.trim();
    trimmed.strip_suffix(';').unwrap_or(trimmed).trim()
}

fn prompt(stdout: &mut impl Write, buffer: &str) -> anyhow::Result<()> {
    if buffer.is_empty() {
        write!(stdout, "simple_rdbms> ")?;
    } else {
        write!(stdout, "        ...> ")?;
    }
    stdout.flush()?;
    Ok(())
}

fn print_tables(db: &Database) {
    for name in db.table_names() {
        println!("{name}");
    }
}

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

fn print_result(result_set: &ResultSet) {
    match result_set {
        ResultSet::Rows { columns, rows } => print_table(columns, rows),
        ResultSet::RowsAffected(0) => println!("OK"),
        ResultSet::RowsAffected(count) => {
            println!("OK ({count} row{})", if *count == 1 { "" } else { "s" })
        }
        ResultSet::RolledBack => println!("ROLLBACK"),
    }
}

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

#[cfg(test)]
mod tests {
    use super::statement_from_buffer;

    #[test]
    fn only_the_terminating_semicolon_is_stripped() {
        let buffer = "SELECT * FROM t WHERE s = ';';\n";
        assert_eq!(statement_from_buffer(buffer), "SELECT * FROM t WHERE s = ';'");
    }

    #[test]
    fn a_statement_without_a_string_literal_still_strips_its_terminator() {
        let buffer = "SELECT * FROM t;\n";
        assert_eq!(statement_from_buffer(buffer), "SELECT * FROM t");
    }
}
