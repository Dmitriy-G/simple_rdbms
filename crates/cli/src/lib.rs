#![forbid(unsafe_code)]

use std::io::{self, BufRead, Write};

use engine::{DataType, Database, ResultSet, Tuple, Value};

pub fn run_repl(db: &mut Database) -> anyhow::Result<()> {
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
            match parse_meta_command(trimmed) {
                Some(MetaCommand::Exit) => break,
                Some(MetaCommand::Tables) => {
                    print_tables(db);
                    prompt(&mut stdout, &buffer)?;
                    continue;
                }
                Some(MetaCommand::Schema(name)) => {
                    print_schema(db, &name);
                    prompt(&mut stdout, &buffer)?;
                    continue;
                }
                None => {}
            }
        }

        buffer.push_str(&line);
        buffer.push('\n');

        if trimmed.ends_with(';') {
            let statement = statement_from_buffer(&buffer);
            match db.execute(statement) {
                Ok(result_set) => print!("{}", format_result(&result_set)),
                Err(err) => eprintln!("error: {err}"),
            }
            buffer.clear();
        }
        prompt(&mut stdout, &buffer)?;
    }

    Ok(())
}

pub fn statement_from_buffer(buffer: &str) -> &str {
    let trimmed = buffer.trim();
    trimmed.strip_suffix(';').unwrap_or(trimmed).trim()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaCommand {
    Exit,
    Tables,
    Schema(String),
}

pub fn parse_meta_command(trimmed: &str) -> Option<MetaCommand> {
    if trimmed.eq_ignore_ascii_case(".exit") {
        return Some(MetaCommand::Exit);
    }
    if trimmed.eq_ignore_ascii_case(".tables") {
        return Some(MetaCommand::Tables);
    }
    if let Some(name) = trimmed.strip_prefix(".schema ") {
        return Some(MetaCommand::Schema(name.trim().to_string()));
    }
    None
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

pub fn format_result(result_set: &ResultSet) -> String {
    match result_set {
        ResultSet::Rows { columns, rows } => format_table(columns, rows),
        ResultSet::RowsAffected(0) => "OK\n".to_string(),
        ResultSet::RowsAffected(count) => {
            format!("OK ({count} row{})\n", if *count == 1 { "" } else { "s" })
        }
        ResultSet::RolledBack => "ROLLBACK\n".to_string(),
    }
}

pub fn format_table(columns: &[String], rows: &[Tuple]) -> String {
    let formatted_rows: Vec<Vec<String>> =
        rows.iter().map(|row| row.values().iter().map(format_value).collect()).collect();

    let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
    for row in &formatted_rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.len());
        }
    }

    let mut out = String::new();
    let header: Vec<String> = columns.iter().zip(&widths).map(|(c, w)| format!("{c:w$}")).collect();
    out.push_str(&header.join(" | "));
    out.push('\n');
    out.push_str(&widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>().join("-+-"));
    out.push('\n');
    for row in &formatted_rows {
        let cells: Vec<String> = row.iter().zip(&widths).map(|(c, w)| format!("{c:w$}")).collect();
        out.push_str(&cells.join(" | "));
        out.push('\n');
    }
    out
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
