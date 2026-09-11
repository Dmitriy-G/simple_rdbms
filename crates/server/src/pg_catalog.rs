use engine::{DataType, Database};
use pgwire::api::Type;
use pgwire::api::results::{FieldFormat, FieldInfo};

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
const FIXED_ROLE_NAME: &str = "postgres";
const DEFAULT_TABLESPACE_OID: u32 = 1663;
const UTF8_ENCODING: u32 = 6;
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

pub(crate) fn pg_type_of(data_type: Option<&DataType>) -> Type {
    match data_type {
        Some(DataType::Boolean) => Type::BOOL,
        Some(DataType::Integer) => Type::INT4,
        Some(DataType::BigInt) => Type::INT8,
        Some(DataType::Double) => Type::FLOAT8,
        Some(DataType::Varchar(_)) | None => Type::VARCHAR,
    }
}

pub(crate) fn field_info(
    name: &str,
    data_type: Option<&DataType>,
    format: FieldFormat,
) -> FieldInfo {
    FieldInfo::new(name.to_string(), None, None, pg_type_of(data_type), format)
}

fn answer_pg_attribute(db: &Database, normalized: &str) -> Option<Introspection> {
    if !recognizes_relation(normalized, "pg_attribute") {
        return None;
    }

    let mut names = db.table_names();
    let clause = where_clause(normalized);
    let attname_filter = clause.and_then(|clause| eq_string_predicate(clause, "attname"));
    if let Some(clause) = clause {
        if let Some(attrelid) = eq_number_predicate(clause, "attrelid") {
            names.retain(|name| u64::from(table_oid(name)) == attrelid);
        }
    }

    let mut rows = Vec::new();
    for name in names {
        let Ok(schema) = db.table_schema(&name) else {
            continue;
        };
        let oid = table_oid(&name);
        for (idx, column) in schema.columns().iter().enumerate() {
            if let Some(filter) = attname_filter {
                if column.name != filter {
                    continue;
                }
            }
            let attnum = idx + 1;
            rows.push(vec![
                Some(oid.to_string()),
                Some(column.name.clone()),
                Some(pg_type_of(Some(&column.data_type)).oid().to_string()),
                Some(attnum.to_string()),
                Some(if column.nullable { "f" } else { "t" }.to_string()),
                Some("-1".to_string()),
                Some("f".to_string()),
                Some(String::new()),
            ]);
        }
    }

    Some(Introspection {
        columns: vec![
            ("attrelid".to_string(), Type::OID),
            ("attname".to_string(), Type::VARCHAR),
            ("atttypid".to_string(), Type::OID),
            ("attnum".to_string(), Type::INT4),
            ("attnotnull".to_string(), Type::VARCHAR),
            ("atttypmod".to_string(), Type::INT4),
            ("attisdropped".to_string(), Type::VARCHAR),
            ("attidentity".to_string(), Type::VARCHAR),
        ],
        rows,
    })
}

fn pg_type_catalog() -> Vec<Type> {
    vec![Type::BOOL, Type::INT4, Type::INT8, Type::FLOAT8, Type::VARCHAR]
}

fn answer_pg_type(normalized: &str) -> Option<Introspection> {
    if !recognizes_relation(normalized, "pg_type") {
        return None;
    }

    let mut types = pg_type_catalog();
    if let Some(clause) = where_clause(normalized) {
        if let Some(oid) = eq_number_predicate(clause, "oid") {
            types.retain(|pg_type| u64::from(pg_type.oid()) == oid);
        }
        if let Some(typname) = eq_string_predicate(clause, "typname") {
            types.retain(|pg_type| pg_type.name() == typname);
        }
    }

    let rows = types
        .into_iter()
        .map(|pg_type| {
            vec![
                Some(pg_type.oid().to_string()),
                Some(pg_type.name().to_string()),
                Some(PG_CATALOG_NAMESPACE_OID.to_string()),
                Some("b".to_string()),
                Some("0".to_string()),
                Some("0".to_string()),
                Some("0".to_string()),
            ]
        })
        .collect();

    Some(Introspection {
        columns: vec![
            ("oid".to_string(), Type::OID),
            ("typname".to_string(), Type::VARCHAR),
            ("typnamespace".to_string(), Type::OID),
            ("typtype".to_string(), Type::VARCHAR),
            ("typelem".to_string(), Type::OID),
            ("typbasetype".to_string(), Type::OID),
            ("typrelid".to_string(), Type::OID),
        ],
        rows,
    })
}

fn bool_cell(value: bool) -> Option<String> {
    Some(if value { "t" } else { "f" }.to_string())
}

fn database_oid(name: &str) -> u32 {
    table_oid(&format!("pg_database.{name}"))
}

fn answer_pg_database(db: &Database, normalized: &str) -> Option<Introspection> {
    if !recognizes_relation(normalized, "pg_database") {
        return None;
    }

    let name = db.database_name().to_string();
    let oid = database_oid(&name);
    let mut rows = vec![vec![
        Some(oid.to_string()),
        Some(name.clone()),
        Some(FIXED_OWNER_OID.to_string()),
        Some(UTF8_ENCODING.to_string()),
        Some("C".to_string()),
        Some("C".to_string()),
        bool_cell(false),
        bool_cell(true),
        Some("-1".to_string()),
        Some(DEFAULT_TABLESPACE_OID.to_string()),
        None,
    ]];

    if let Some(clause) = where_clause(normalized) {
        if let Some(datname) = eq_string_predicate(clause, "datname") {
            if datname != name {
                rows.clear();
            }
        }
        if let Some(filter) = eq_number_predicate(clause, "oid") {
            if filter != u64::from(oid) {
                rows.clear();
            }
        }
    }

    Some(Introspection {
        columns: vec![
            ("oid".to_string(), Type::OID),
            ("datname".to_string(), Type::VARCHAR),
            ("datdba".to_string(), Type::OID),
            ("encoding".to_string(), Type::INT4),
            ("datcollate".to_string(), Type::VARCHAR),
            ("datctype".to_string(), Type::VARCHAR),
            ("datistemplate".to_string(), Type::BOOL),
            ("datallowconn".to_string(), Type::BOOL),
            ("datconnlimit".to_string(), Type::INT4),
            ("dattablespace".to_string(), Type::OID),
            ("datacl".to_string(), Type::VARCHAR),
        ],
        rows,
    })
}

fn answer_pg_roles(normalized: &str) -> Option<Introspection> {
    if !recognizes_relation(normalized, "pg_roles") {
        return None;
    }

    let mut rows = vec![vec![
        Some(FIXED_OWNER_OID.to_string()),
        Some(FIXED_ROLE_NAME.to_string()),
        bool_cell(true),
        bool_cell(true),
        bool_cell(true),
        bool_cell(true),
        bool_cell(true),
        bool_cell(true),
        bool_cell(true),
        Some("-1".to_string()),
        None,
        None,
    ]];

    if let Some(clause) = where_clause(normalized) {
        if let Some(rolname) = eq_string_predicate(clause, "rolname") {
            if rolname != FIXED_ROLE_NAME {
                rows.clear();
            }
        }
        if let Some(filter) = eq_number_predicate(clause, "oid") {
            if filter != u64::from(FIXED_OWNER_OID) {
                rows.clear();
            }
        }
    }

    Some(Introspection {
        columns: vec![
            ("oid".to_string(), Type::OID),
            ("rolname".to_string(), Type::VARCHAR),
            ("rolsuper".to_string(), Type::BOOL),
            ("rolinherit".to_string(), Type::BOOL),
            ("rolcreaterole".to_string(), Type::BOOL),
            ("rolcreatedb".to_string(), Type::BOOL),
            ("rolcanlogin".to_string(), Type::BOOL),
            ("rolreplication".to_string(), Type::BOOL),
            ("rolbypassrls".to_string(), Type::BOOL),
            ("rolconnlimit".to_string(), Type::INT4),
            ("rolvaliduntil".to_string(), Type::VARCHAR),
            ("rolconfig".to_string(), Type::VARCHAR),
        ],
        rows,
    })
}

fn answer_pg_user(normalized: &str) -> Option<Introspection> {
    if !recognizes_relation(normalized, "pg_user") {
        return None;
    }

    let mut rows = vec![vec![
        Some(FIXED_ROLE_NAME.to_string()),
        Some(FIXED_OWNER_OID.to_string()),
        bool_cell(true),
        bool_cell(true),
        bool_cell(true),
        Some("********".to_string()),
        None,
        None,
    ]];

    if let Some(clause) = where_clause(normalized) {
        if let Some(usename) = eq_string_predicate(clause, "usename") {
            if usename != FIXED_ROLE_NAME {
                rows.clear();
            }
        }
        if let Some(filter) = eq_number_predicate(clause, "usesysid") {
            if filter != u64::from(FIXED_OWNER_OID) {
                rows.clear();
            }
        }
    }

    Some(Introspection {
        columns: vec![
            ("usename".to_string(), Type::VARCHAR),
            ("usesysid".to_string(), Type::OID),
            ("usecreatedb".to_string(), Type::BOOL),
            ("usesuper".to_string(), Type::BOOL),
            ("userepl".to_string(), Type::BOOL),
            ("passwd".to_string(), Type::VARCHAR),
            ("valuntil".to_string(), Type::VARCHAR),
            ("useconfig".to_string(), Type::VARCHAR),
        ],
        rows,
    })
}

fn is_pg_relation(token: &str) -> bool {
    token.strip_prefix("pg_catalog.").unwrap_or(token).starts_with("pg_")
}

fn select_list(normalized: &str) -> Option<&str> {
    let rest = normalized.strip_prefix("select ")?;
    let idx = rest.find(" from ")?;
    Some(rest[..idx].trim())
}

fn split_select_items(select_list: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut start = 0;
    for (idx, c) in select_list.char_indices() {
        if in_string {
            if c == '\'' {
                in_string = false;
            }
            continue;
        }
        match c {
            '\'' => in_string = true,
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                items.push(select_list[start..idx].trim());
                start = idx + 1;
            }
            _ => {}
        }
    }
    items.push(select_list[start..].trim());
    items
}

fn is_simple_identifier(item: &str) -> bool {
    !item.is_empty()
        && item.split('.').all(|part| {
            !part.is_empty()
                && part.chars().enumerate().all(|(idx, c)| {
                    if idx == 0 {
                        c.is_ascii_alphabetic() || c == '_'
                    } else {
                        c.is_ascii_alphanumeric() || c == '_'
                    }
                })
        })
}

fn select_item_name(item: &str) -> String {
    if let Some(pos) = item.rfind(" as ") {
        let alias = item[pos + " as ".len()..].trim();
        if !alias.is_empty() {
            return alias.to_string();
        }
    }
    if item != "*" && is_simple_identifier(item) {
        return item.rsplit('.').next().unwrap_or(item).to_string();
    }
    "?column?".to_string()
}

fn select_list_columns(normalized: &str) -> Vec<(String, Type)> {
    let Some(list) = select_list(normalized) else {
        return vec![("?column?".to_string(), Type::VARCHAR)];
    };
    split_select_items(list)
        .into_iter()
        .map(|item| (select_item_name(item), Type::VARCHAR))
        .collect()
}

fn answer_unrecognized_pg_relation(normalized: &str) -> Option<Introspection> {
    let relation = relation_after_from(normalized)?;
    if !is_pg_relation(relation) {
        return None;
    }
    Some(Introspection { columns: select_list_columns(normalized), rows: Vec::new() })
}

fn shadows_a_real_table(db: &Database, normalized: &str) -> bool {
    let Some(relation) = relation_after_from(normalized) else {
        return false;
    };
    if relation.starts_with("pg_catalog.") || !is_pg_relation(relation) {
        return false;
    }
    db.table_names().iter().any(|name| name.eq_ignore_ascii_case(relation))
}

pub fn answer(db: &Database, sql: &str) -> Option<Introspection> {
    let normalized = normalize(sql);
    if shadows_a_real_table(db, &normalized) {
        return None;
    }
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
    if let Some(introspection) = answer_pg_attribute(db, &normalized) {
        return Some(introspection);
    }
    if let Some(introspection) = answer_pg_type(&normalized) {
        return Some(introspection);
    }
    if let Some(introspection) = answer_pg_database(db, &normalized) {
        return Some(introspection);
    }
    if let Some(introspection) = answer_pg_roles(&normalized) {
        return Some(introspection);
    }
    if let Some(introspection) = answer_pg_user(&normalized) {
        return Some(introspection);
    }
    if let Some(introspection) = answer_unrecognized_pg_relation(&normalized) {
        return Some(introspection);
    }
    None
}
