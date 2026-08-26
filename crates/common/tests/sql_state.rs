use common::{Error, SqlState};

fn sample_errors() -> Vec<Error> {
    vec![
        Error::Io(std::io::Error::other("disk gone")),
        Error::ChecksumMismatch { page_id: 1, expected: 1, actual: 2 },
        Error::CorruptPage { page_id: 1, reason: "bad magic".to_string() },
        Error::CorruptLog { reason: "truncated header".to_string() },
        Error::TruncatedFile { actual: 100, expected: 4096, page_size: 4096 },
        Error::PageNotFound { page_id: 7 },
        Error::DoubleWriteRestoreFailed { page_id: 3 },
        Error::BufferPoolExhausted,
        Error::TupleTooLarge { size: 5000, max: 4000 },
        Error::KeyTooLarge { size: 5000, max: 4000 },
        Error::InvalidConfiguration { detail: "dwb capacity must be >= 1".to_string() },
        Error::Syntax { message: "unexpected token".to_string(), offset: 12 },
        Error::ColumnCountMismatch { expected: 2, found: 3 },
        Error::UndefinedTable { name: "missing".to_string() },
        Error::UndefinedColumn { name: "ghost".to_string() },
        Error::AmbiguousColumn { name: "id".to_string() },
        Error::DuplicateTable { name: "users".to_string() },
        Error::UndefinedIndex { name: "idx_missing".to_string() },
        Error::DuplicateIndex { name: "idx_users_id".to_string() },
        Error::DatatypeMismatch { detail: "expected INTEGER".to_string() },
        Error::NumericValueOutOfRange {
            column: "a".to_string(),
            value: "99999999999".to_string(),
            data_type: "Integer".to_string(),
        },
        Error::DataCorrupted { detail: "unknown column type tag".to_string() },
        Error::SerializationFailure { detail: "deadlock".to_string() },
        Error::Internal { detail: "unknown transaction".to_string() },
        Error::NestedTransaction,
        Error::NoActiveTransaction { statement: "COMMIT".to_string() },
        Error::TransactionAborted,
        Error::NotSupported("arithmetic".to_string()),
    ]
}

fn expected_sql_state(err: &Error) -> SqlState {
    match err {
        Error::Io(_) => SqlState::IO_ERROR,
        Error::ChecksumMismatch { .. } => SqlState::DATA_CORRUPTED,
        Error::CorruptPage { .. } => SqlState::DATA_CORRUPTED,
        Error::CorruptLog { .. } => SqlState::DATA_CORRUPTED,
        Error::TruncatedFile { .. } => SqlState::DATA_CORRUPTED,
        Error::PageNotFound { .. } => SqlState::DATA_CORRUPTED,
        Error::DoubleWriteRestoreFailed { .. } => SqlState::IO_ERROR,
        Error::BufferPoolExhausted => SqlState::OUT_OF_MEMORY,
        Error::TupleTooLarge { .. } => SqlState::PROGRAM_LIMIT_EXCEEDED,
        Error::KeyTooLarge { .. } => SqlState::PROGRAM_LIMIT_EXCEEDED,
        Error::InvalidConfiguration { .. } => SqlState::CONNECTION_FAILURE,
        Error::Syntax { .. } => SqlState::SYNTAX_ERROR,
        Error::ColumnCountMismatch { .. } => SqlState::SYNTAX_ERROR,
        Error::UndefinedTable { .. } => SqlState::UNDEFINED_TABLE,
        Error::UndefinedColumn { .. } => SqlState::UNDEFINED_COLUMN,
        Error::AmbiguousColumn { .. } => SqlState::AMBIGUOUS_COLUMN,
        Error::DuplicateTable { .. } => SqlState::DUPLICATE_TABLE,
        Error::UndefinedIndex { .. } => SqlState::UNDEFINED_OBJECT,
        Error::DuplicateIndex { .. } => SqlState::DUPLICATE_OBJECT,
        Error::DatatypeMismatch { .. } => SqlState::DATATYPE_MISMATCH,
        Error::NumericValueOutOfRange { .. } => SqlState::NUMERIC_VALUE_OUT_OF_RANGE,
        Error::DataCorrupted { .. } => SqlState::DATA_CORRUPTED,
        Error::SerializationFailure { .. } => SqlState::SERIALIZATION_FAILURE,
        Error::Internal { .. } => SqlState::INTERNAL_ERROR,
        Error::NestedTransaction => SqlState::NO_ACTIVE_SQL_TRANSACTION,
        Error::NoActiveTransaction { .. } => SqlState::NO_ACTIVE_SQL_TRANSACTION,
        Error::TransactionAborted => SqlState::IN_FAILED_SQL_TRANSACTION,
        Error::NotSupported(_) => SqlState::FEATURE_NOT_SUPPORTED,
    }
}

#[test]
fn every_error_variant_maps_to_its_sql_state() {
    for err in sample_errors() {
        assert_eq!(
            err.sql_state(),
            expected_sql_state(&err),
            "wrong SQLSTATE for {err:?}: got {}, expected {}",
            err.sql_state(),
            expected_sql_state(&err)
        );
    }
}

#[test]
fn is_retryable_is_true_only_for_class_40_codes() {
    for err in sample_errors() {
        let is_class_40 = matches!(
            err.sql_state(),
            SqlState::SERIALIZATION_FAILURE | SqlState::STATEMENT_COMPLETION_UNKNOWN
        );
        assert_eq!(err.is_retryable(), is_class_40, "wrong is_retryable for {err:?}");
    }
}
