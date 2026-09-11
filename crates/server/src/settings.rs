pub const TRANSACTION_ISOLATION: &str = "transaction_isolation";

pub const SETTINGS: &[(&str, &str)] = &[
    ("client_encoding", "UTF8"),
    ("DateStyle", "ISO, MDY"),
    ("integer_datetimes", "on"),
    ("IntervalStyle", "postgres"),
    ("is_superuser", "on"),
    ("max_identifier_length", "63"),
    ("max_index_keys", "32"),
    ("server_encoding", "UTF8"),
    ("server_version", "15.0"),
    ("server_version_num", "150000"),
    ("standard_conforming_strings", "on"),
    (TRANSACTION_ISOLATION, "read committed"),
    ("TimeZone", "UTC"),
];

pub fn lookup(name: &str) -> Option<&'static str> {
    SETTINGS.iter().find(|(setting, _)| setting.eq_ignore_ascii_case(name)).map(|(_, value)| *value)
}

pub fn parameter_name(words: &[&str]) -> String {
    let joined = words.join(" ");
    if joined.eq_ignore_ascii_case("transaction isolation level") {
        return TRANSACTION_ISOLATION.to_string();
    }
    words.first().map(|word| (*word).to_string()).unwrap_or_default()
}
