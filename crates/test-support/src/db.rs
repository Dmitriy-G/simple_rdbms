use std::path::Path;

use common::DbConfig;

pub fn db_config(dir: &Path) -> DbConfig {
    DbConfig::new(dir.join("test.db"))
}
