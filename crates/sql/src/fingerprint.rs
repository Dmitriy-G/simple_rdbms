use crate::lexer::Lexer;
use crate::token::TokenKind;

pub fn fingerprint(sql: &str) -> String {
    let Ok(tokens) = Lexer::new(sql).tokenize() else {
        return "?".to_string();
    };

    let mut out = String::with_capacity(sql.len());
    let mut cursor = 0usize;
    for i in 0..tokens.len().saturating_sub(1) {
        let start = tokens[i].offset;
        out.push_str(&sql[cursor..start]);

        let mut end = tokens[i + 1].offset;
        while end > start && sql.as_bytes()[end - 1].is_ascii_whitespace() {
            end -= 1;
        }

        if is_literal(&tokens[i].kind) {
            out.push('?');
        } else {
            out.push_str(&sql[start..end]);
        }
        cursor = end;
    }
    out.push_str(&sql[cursor..]);
    out
}

fn is_literal(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::IntegerLiteral(_) | TokenKind::FloatLiteral(_) | TokenKind::StringLiteral(_)
    )
}
