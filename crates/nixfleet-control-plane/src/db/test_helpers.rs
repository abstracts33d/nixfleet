//! Cross-module test fixtures.

use super::Db;

pub(crate) fn fresh_db() -> Db {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    db
}
