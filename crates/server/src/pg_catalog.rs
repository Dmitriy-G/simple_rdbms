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

const PG_CATALOG_NAMESPACE_OID: u32 = 11;
const PUBLIC_NAMESPACE_OID: u32 = 2200;
const FIXED_OWNER_OID: u32 = 10;
const TABLE_OID_BASE: u32 = 16384;

fn fnv1a_32(input: &str) -> u32 {
    const FNV_OFFSET_BASIS: u32 = 0x811c_9dc5;
    const FNV_PRIME: u32 = 0x0100_0193;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in input.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn table_oid(name: &str) -> u32 {
    TABLE_OID_BASE + (fnv1a_32(name) % (u32::MAX - TABLE_OID_BASE))
}

fn relation_after_from(normalized: &str) -> Option<&str> {
    let mut tokens = normalized.split(' ');
    while let Some(token) = tokens.next() {
        if token == "from" {
            return tokens.next();
        }
    }
    None
}

fn relation_matches(token: &str, relation: &str) -> bool {
    token == relation || token == format!("pg_catalog.{relation}")
}

fn recognizes_relation(normalized: &str, relation: &str) -> bool {
    relation_after_from(normalized).is_some_and(|token| relation_matches(token, relation))
}

fn where_clause(normalized: &str) -> Option<&str> {
    let idx = normalized.find(" where ")?;
    Some(normalized[idx + " where ".len()..].trim())
}

fn find_column_token(clause: &str, column: &str) -> Option<usize> {
    let bytes = clause.as_bytes();
    let mut search_from = 0;
    while let Some(rel_pos) = clause[search_from..].find(column) {
        let pos = search_from + rel_pos;
        let end = pos + column.len();
        let before_ok = pos == 0 || {
            let prev = bytes[pos - 1] as char;
            !(prev.is_alphanumeric() || prev == '_')
        };
        let after_ok = end == bytes.len() || {
            let next = bytes[end] as char;
            !(next.is_alphanumeric() || next == '_')
        };
        if before_ok && after_ok {
            return Some(end);
        }
        search_from = end;
    }
    None
}

fn eq_string_predicate<'a>(clause: &'a str, column: &str) -> Option<&'a str> {
    let end = find_column_token(clause, column)?;
    let rest = clause[end..].trim_start().strip_prefix('=')?.trim_start();
    let rest = rest.strip_prefix('\'')?;
    let value_end = rest.find('\'')?;
    Some(&rest[..value_end])
}

fn eq_number_predicate(clause: &str, column: &str) -> Option<u64> {
    let end = find_column_token(clause, column)?;
    let rest = clause[end..].trim_start().strip_prefix('=')?.trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() { None } else { digits.parse().ok() }
}

fn relkind_in_list(clause: &str) -> Option<Vec<&str>> {
    let end = find_column_token(clause, "relkind")?;
    let rest = clause[end..].trim_start().strip_prefix("in")?.trim_start();
    let rest = rest.strip_prefix('(')?;
    let close = rest.find(')')?;
    Some(
        rest[..close]
            .split(',')
            .filter_map(|item| {
                let item = item.trim();
                item.strip_prefix('\'').and_then(|s| s.strip_suffix('\''))
            })
            .collect(),
    )
}

fn answer_pg_namespace(normalized: &str) -> Option<Introspection> {
    if !recognizes_relation(normalized, "pg_namespace") {
        return None;
    }
    let rows = vec![
        vec![
            Some(PG_CATALOG_NAMESPACE_OID.to_string()),
            Some("pg_catalog".to_string()),
            Some(FIXED_OWNER_OID.to_string()),
            None,
        ],
        vec![
            Some(PUBLIC_NAMESPACE_OID.to_string()),
            Some("public".to_string()),
            Some(FIXED_OWNER_OID.to_string()),
            None,
        ],
    ];
    Some(Introspection {
        columns: vec![
            ("oid".to_string(), Type::OID),
            ("nspname".to_string(), Type::VARCHAR),
            ("nspowner".to_string(), Type::OID),
            ("nspacl".to_string(), Type::VARCHAR),
        ],
        rows,
    })
}

fn answer_pg_class(db: &Database, normalized: &str) -> Option<Introspection> {
    if !recognizes_relation(normalized, "pg_class") {
        return None;
    }

    let mut names = db.table_names();
    if let Some(clause) = where_clause(normalized) {
        if let Some(kinds) = relkind_in_list(clause) {
            if !kinds.contains(&"r") {
                names.clear();
            }
        }
        if let Some(nspname) = eq_string_predicate(clause, "nspname") {
            if nspname != "public" {
                names.clear();
            }
        }
        if let Some(relnamespace) = eq_number_predicate(clause, "relnamespace") {
            if relnamespace != u64::from(PUBLIC_NAMESPACE_OID) {
                names.clear();
            }
        }
        if let Some(relname) = eq_string_predicate(clause, "relname") {
            names.retain(|name| name == relname);
        }
        if let Some(oid) = eq_number_predicate(clause, "oid") {
            names.retain(|name| u64::from(table_oid(name)) == oid);
        }
    }

    let rows = names
        .into_iter()
        .map(|name| {
            let relnatts = db.table_schema(&name).map(|schema| schema.columns().len()).unwrap_or(0);
            let oid = table_oid(&name);
            vec![
                Some(oid.to_string()),
                Some(name),
                Some(PUBLIC_NAMESPACE_OID.to_string()),
                Some("r".to_string()),
                Some(relnatts.to_string()),
                Some("-1".to_string()),
                Some("f".to_string()),
                Some("p".to_string()),
                Some("0".to_string()),
            ]
        })
        .collect();

    Some(Introspection {
        columns: vec![
            ("oid".to_string(), Type::OID),
            ("relname".to_string(), Type::VARCHAR),
            ("relnamespace".to_string(), Type::OID),
            ("relkind".to_string(), Type::VARCHAR),
            ("relnatts".to_string(), Type::INT4),
            ("reltuples".to_string(), Type::VARCHAR),
            ("relhasindex".to_string(), Type::VARCHAR),
            ("relpersistence".to_string(), Type::VARCHAR),
            ("relam".to_string(), Type::OID),
        ],
        rows,
    })
}

pub fn answer(db: &Database, sql: &str) -> Option<Introspection> {
    let normalized = normalize(sql);
    if is_select_version(&normalized) {
        return Some(Introspection {
            columns: vec![("version".to_string(), Type::VARCHAR)],
            rows: vec![version_row()],
        });
    }
    if let Some(introspection) = answer_pg_namespace(&normalized) {
        return Some(introspection);
    }
    if let Some(introspection) = answer_pg_class(db, &normalized) {
        return Some(introspection);
    }
    None
}
