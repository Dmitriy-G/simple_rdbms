use std::error::Error;

use common::{DbConfig, Error as CommonError, SqlState};
use engine::Database;

#[test]
fn a_second_database_open_reports_a_usable_error() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let config = DbConfig::new(dir.path().join("locked.db"));

    let _first = Database::open(config.clone())?;

    match Database::open(config) {
        Err(err @ CommonError::DatabaseLocked { .. }) => {
            assert_eq!(err.sql_state(), SqlState::LOCK_NOT_AVAILABLE);
        }
        Err(other) => panic!("expected DatabaseLocked, got {other}"),
        Ok(_) => panic!("a second open of a locked database must be refused"),
    }
    Ok(())
}
