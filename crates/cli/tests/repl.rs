use std::error::Error;

use cli::{MetaCommand, format_table, parse_meta_command, statement_from_buffer};
use engine::{Tuple, Value};

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

#[test]
fn exit_is_recognized_case_insensitively() {
    assert_eq!(parse_meta_command(".EXIT"), Some(MetaCommand::Exit));
}

#[test]
fn tables_is_recognized() {
    assert_eq!(parse_meta_command(".tables"), Some(MetaCommand::Tables));
}

#[test]
fn schema_captures_the_trimmed_table_name() {
    assert_eq!(parse_meta_command(".schema   t  "), Some(MetaCommand::Schema("t".to_string())));
}

#[test]
fn an_ordinary_statement_is_not_a_meta_command() {
    assert_eq!(parse_meta_command("SELECT * FROM t;"), None);
}

#[test]
fn null_values_render_as_the_literal_text_null_in_the_output_table() -> Result<(), Box<dyn Error>> {
    let columns = vec!["a".to_string(), "b".to_string()];
    let rows = vec![Tuple::new(vec![Value::Integer(1), Value::Null])];

    let table = format_table(&columns, &rows);
    let data_line = table.lines().nth(2).ok_or("expected a data row after the header/separator")?;
    assert_eq!(data_line, "1 | NULL");
    Ok(())
}
