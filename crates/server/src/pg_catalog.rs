use engine::Database;
use pgwire::api::Type;

pub struct Introspection {
    pub columns: Vec<(String, Type)>,
    pub rows: Vec<Vec<Option<String>>>,
}

pub fn normalize(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut in_string = false;
    let mut pending_space = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\'' {
                in_string = false;
            }
            continue;
        }

        if c == '\'' {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            in_string = true;
            out.push(c);
            continue;
        }

        if c == '-' && chars.peek() == Some(&'-') {
            chars.next();
            for c2 in chars.by_ref() {
                if c2 == '\n' {
                    break;
                }
            }
            pending_space = true;
            continue;
        }

        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut prev = '\0';
            for c2 in chars.by_ref() {
                if prev == '*' && c2 == '/' {
                    break;
                }
                prev = c2;
            }
            pending_space = true;
            continue;
        }

        if c.is_whitespace() {
            pending_space = true;
            continue;
        }

        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        pending_space = false;
        out.extend(c.to_lowercase());
    }

    out.trim_end_matches(';').trim().to_string()
}

fn is_select_version(normalized: &str) -> bool {
    let Some(rest) = normalized.strip_prefix("select version()") else {
        return false;
    };
    let rest = rest.trim();
    if rest.is_empty() {
        return true;
    }
    let rest = rest.strip_prefix("as ").unwrap_or(rest);
    !rest.is_empty() && !rest.contains(' ')
}

fn version_row() -> Vec<Option<String>> {
    vec![Some(format!("PostgreSQL 15.0 (simple_rdbms {})", env!("CARGO_PKG_VERSION")))]
}

pub fn answer(_db: &Database, sql: &str) -> Option<Introspection> {
    let normalized = normalize(sql);
    if is_select_version(&normalized) {
        return Some(Introspection {
            columns: vec![("version".to_string(), Type::VARCHAR)],
            rows: vec![version_row()],
        });
    }
    None
}
