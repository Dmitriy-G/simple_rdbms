#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SqlState(pub [u8; 5]);

impl SqlState {
    pub const SUCCESSFUL_COMPLETION: SqlState = SqlState(*b"00000");

    pub const CONNECTION_FAILURE: SqlState = SqlState(*b"08006");

    pub const NUMERIC_VALUE_OUT_OF_RANGE: SqlState = SqlState(*b"22003");
    pub const NULL_VALUE_NOT_ALLOWED: SqlState = SqlState(*b"22004");
    pub const INVALID_TEXT_REPRESENTATION: SqlState = SqlState(*b"22P02");

    pub const NOT_NULL_VIOLATION: SqlState = SqlState(*b"23502");
    pub const UNIQUE_VIOLATION: SqlState = SqlState(*b"23505");

    pub const NO_ACTIVE_SQL_TRANSACTION: SqlState = SqlState(*b"25P01");
    pub const IN_FAILED_SQL_TRANSACTION: SqlState = SqlState(*b"25P02");

    pub const LOCK_NOT_AVAILABLE: SqlState = SqlState(*b"55P03");

    pub const SERIALIZATION_FAILURE: SqlState = SqlState(*b"40001");
    pub const STATEMENT_COMPLETION_UNKNOWN: SqlState = SqlState(*b"40003");

    pub const SYNTAX_ERROR: SqlState = SqlState(*b"42601");
    pub const AMBIGUOUS_COLUMN: SqlState = SqlState(*b"42702");
    pub const UNDEFINED_COLUMN: SqlState = SqlState(*b"42703");
    pub const UNDEFINED_TABLE: SqlState = SqlState(*b"42P01");
    pub const DUPLICATE_TABLE: SqlState = SqlState(*b"42P07");
    pub const DATATYPE_MISMATCH: SqlState = SqlState(*b"42804");
    pub const UNDEFINED_OBJECT: SqlState = SqlState(*b"42704");
    pub const DUPLICATE_OBJECT: SqlState = SqlState(*b"42710");

    pub const FEATURE_NOT_SUPPORTED: SqlState = SqlState(*b"0A000");

    pub const DISK_FULL: SqlState = SqlState(*b"53100");
    pub const OUT_OF_MEMORY: SqlState = SqlState(*b"53200");
    pub const PROGRAM_LIMIT_EXCEEDED: SqlState = SqlState(*b"54000");

    pub const QUERY_CANCELED: SqlState = SqlState(*b"57014");
    pub const ADMIN_SHUTDOWN: SqlState = SqlState(*b"57P01");

    pub const IO_ERROR: SqlState = SqlState(*b"58030");

    pub const INTERNAL_ERROR: SqlState = SqlState(*b"XX000");
    pub const DATA_CORRUPTED: SqlState = SqlState(*b"XX001");
    pub const INDEX_CORRUPTED: SqlState = SqlState(*b"XX002");

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("?????")
    }

    pub fn class(&self) -> &str {
        &self.as_str()[..2]
    }
}

impl std::fmt::Display for SqlState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
