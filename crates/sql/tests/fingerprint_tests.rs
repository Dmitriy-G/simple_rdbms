use sql::fingerprint;

#[test]
fn integer_and_string_literals_are_redacted() {
    assert_eq!(fingerprint("INSERT INTO t VALUES (42, 'alice')"), "INSERT INTO t VALUES (?, ?)");
}

#[test]
fn a_float_literal_in_a_where_clause_is_redacted() {
    assert_eq!(fingerprint("SELECT * FROM t WHERE a = 3.5"), "SELECT * FROM t WHERE a = ?");
}

#[test]
fn identifiers_keywords_and_punctuation_are_preserved_exactly() {
    let sql = "SELECT  a,   b FROM t WHERE a = 1 AND b <> 2";
    assert_eq!(fingerprint(sql), "SELECT  a,   b FROM t WHERE a = ? AND b <> ?");
}

#[test]
fn a_statement_with_no_literals_is_unchanged() {
    let sql = "SELECT * FROM t WHERE a = b";
    assert_eq!(fingerprint(sql), sql);
}

#[test]
fn a_string_literal_containing_a_comma_and_parens_is_redacted_as_one_token() {
    assert_eq!(fingerprint("INSERT INTO t VALUES ('a, (b) c')"), "INSERT INTO t VALUES (?)");
}

#[test]
fn unlexable_input_redacts_to_a_single_placeholder_rather_than_leaking_a_fragment() {
    assert_eq!(fingerprint("INSERT INTO t VALUES ('unterminated"), "?");
}
