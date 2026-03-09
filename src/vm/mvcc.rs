// C1: MVCC Undo Log
//
// Each DML operation within an explicit transaction appends an UndoEntry.
// On ROLLBACK, entries are replayed in reverse order to undo changes.
// On COMMIT, the log is simply cleared.

use crate::types::Row;

/// One undo-able operation recorded before or after a DML statement.
#[derive(Debug, Clone)]
pub enum UndoEntry {
    /// INSERT was performed — undo by deleting rowid from the table
    Insert { table: String, rowid: i64 },
    /// UPDATE was performed — undo by writing the old row back
    Update {
        table: String,
        rowid: i64,
        old_row: Row,
    },
    /// DELETE was performed — undo by re-inserting the row
    Delete {
        table: String,
        rowid: i64,
        old_row: Row,
    },
}
