//! Background SQLite-state timers — prune of stale rows
//! (`prune_timer`) and rollback-deadline enforcement
//! (`rollback_timer`).

pub mod prune_timer;
pub mod rollback_timer;
