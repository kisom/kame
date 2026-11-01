//! SQLite-backed persistence for a single [`Document`].
//!
//! The storage layer keeps exactly one document: the whole [`Document`] is
//! serialized to JSON and stored in a single row of the `documents` table.
//! WAL journalling plus transactional writes mean an interrupted save leaves
//! the previously committed document intact.

use std::fmt;
use std::path::Path;

use rusqlite::{params, Connection};

use crate::edit::Op;
use crate::model::Document;

/// Everything that can go wrong in the storage layer.
///
/// [`DbError::NoDocument`] is deliberately distinct from the failure variants:
/// callers treat "nothing saved yet" as a first launch and create a default
/// document, whereas a [`DbError::Sqlite`] or [`DbError::Json`] failure means
/// the database is unusable and must be surfaced.
#[derive(Debug)]
pub enum DbError {
    /// No document has been saved yet (empty database).
    NoDocument,
    /// A rusqlite/SQLite failure, propagated unchanged.
    Sqlite(rusqlite::Error),
    /// The stored JSON could not be (de)serialized.
    Json(serde_json::Error),
    /// The database was written by a newer build than this one.
    SchemaTooNew { found: i64, supported: i64 },
    /// WAL journalling could not be enabled.
    JournalMode(String),
}

/// Convenience alias for [`DbError`]; kept for interface compatibility.
///
/// This is a concrete error type, not a boxed trait object: callers can match
/// on it to tell "no document yet" apart from a real failure.
pub type Error = DbError;

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::NoDocument => f.write_str("no document stored"),
            DbError::Sqlite(err) => write!(f, "sqlite error: {err}"),
            DbError::Json(err) => write!(f, "json error: {err}"),
            DbError::SchemaTooNew { found, supported } => write!(
                f,
                "database schema newer than this build \
                 (found version {found}, this build supports {supported})"
            ),
            DbError::JournalMode(mode) => {
                write!(f, "failed to enable WAL journal mode (got {mode:?})")
            }
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DbError::Sqlite(err) => Some(err),
            DbError::Json(err) => Some(err),
            DbError::NoDocument | DbError::SchemaTooNew { .. } | DbError::JournalMode(_) => None,
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(err: rusqlite::Error) -> Self {
        DbError::Sqlite(err)
    }
}

impl From<serde_json::Error> for DbError {
    fn from(err: serde_json::Error) -> Self {
        DbError::Json(err)
    }
}

/// Schema version recorded in `PRAGMA user_version`.
///
/// The `ops` table was added within version 1: it is created with
/// `CREATE TABLE IF NOT EXISTS` on every open, so a database written by the
/// pre-ops build gains it in place without a version bump or migration.
const SCHEMA_VERSION: i64 = 1;

/// The whole document lives in the single row `id = 1`; every edit that
/// produced it is appended to `ops`.
const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS documents (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ops (
    seq  INTEGER PRIMARY KEY AUTOINCREMENT,
    json TEXT NOT NULL
)";

/// A handle to the document database.
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Opens (creating if absent) the database at `path`, enables WAL
    /// journalling, and applies the schema for schema version 1.
    ///
    /// Fails if the database was created by a newer schema version, rather
    /// than silently stamping it back down to [`SCHEMA_VERSION`].
    pub fn open(path: &Path) -> Result<Db, DbError> {
        let conn = Connection::open(path)?;

        // `PRAGMA journal_mode` returns the resulting mode as a row, so it has
        // to be read rather than executed.
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(DbError::JournalMode(mode));
        }

        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(DbError::SchemaTooNew {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }

        conn.execute_batch(SCHEMA)?;
        if version == 0 {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }

        Ok(Db { conn })
    }

    /// Reads the stored document. Errors if no document has been saved yet.
    ///
    /// Only `QueryReturnedNoRows` becomes [`DbError::NoDocument`]; every other
    /// failure (missing table, I/O, locking) is propagated unchanged.
    pub fn load_document(&self) -> Result<Document, DbError> {
        let json: String = self
            .conn
            .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
                row.get(0)
            })
            .map_err(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => DbError::NoDocument,
                other => DbError::Sqlite(other),
            })?;
        let doc = serde_json::from_str(&json)?;
        Ok(doc)
    }

    /// Writes the document in a single transaction, replacing any prior one.
    pub fn save_document(&mut self, doc: &Document) -> Result<(), DbError> {
        let json = serde_json::to_string(doc)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO documents (id, json) VALUES (1, ?1)",
            params![json],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Appends `op` to the durable op log and returns its sequence number.
    ///
    /// Sequence numbers are strictly increasing; they are what
    /// [`Db::truncate_ops_after`] cuts at. A failed insert leaves the log
    /// unchanged (single-statement transaction).
    pub fn append_op(&mut self, op: &Op) -> Result<i64, DbError> {
        let json = serde_json::to_string(op)?;
        self.conn
            .execute("INSERT INTO ops (json) VALUES (?1)", params![json])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Reads the whole op log in application order: `(seq, op)`, oldest first.
    pub fn load_ops(&self) -> Result<Vec<(i64, Op)>, DbError> {
        let mut stmt = self.conn.prepare("SELECT seq, json FROM ops ORDER BY seq")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut ops = Vec::new();
        for row in rows {
            let (seq, json) = row?;
            ops.push((seq, serde_json::from_str(&json)?));
        }
        Ok(ops)
    }

    /// Deletes every op with a sequence number greater than `seq`.
    ///
    /// Called after an undo when a new edit replaces the redo tail, so the log
    /// stays a single linear history.
    pub fn truncate_ops_after(&mut self, seq: i64) -> Result<(), DbError> {
        self.conn
            .execute("DELETE FROM ops WHERE seq > ?1", params![seq])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Document;
    use std::path::{Path, PathBuf};
    use uuid::Uuid;

    fn temp_db_path() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kame-db-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("kame.db")
    }

    fn sample_document() -> Document {
        let mut d = Document::new("Sample");
        d.blocks[0].text = "hello world".to_string();
        d
    }

    #[test]
    fn open_save_load_round_trip() {
        let path = temp_db_path();
        let d = sample_document();
        let mut db = Db::open(&path).unwrap();
        db.save_document(&d).unwrap();
        assert_eq!(db.load_document().unwrap(), d);
    }

    #[test]
    fn interrupted_tx_rolls_back() {
        let path = temp_db_path();
        let committed = sample_document();

        let mut db = Db::open(&path).unwrap();
        db.save_document(&committed).unwrap();

        // Begin a transaction that overwrites the document but never commits.
        let mut uncommitted = committed.clone();
        uncommitted.title = "Uncommitted".to_string();
        db.conn.execute("BEGIN", []).unwrap();
        db.conn
            .execute(
                "INSERT OR REPLACE INTO documents (id, json) VALUES (1, ?1)",
                rusqlite::params![serde_json::to_string(&uncommitted).unwrap()],
            )
            .unwrap();

        // Simulate a crash mid-save: close the connection with the transaction
        // still open, without committing it.
        drop(db);

        let db = Db::open(&path).unwrap();
        assert_eq!(db.load_document().unwrap(), committed);
    }

    /// `save_document` itself must be atomic: if its own write fails partway,
    /// the previously committed row survives untouched.
    #[test]
    fn save_document_failure_keeps_previous_row() {
        let path = temp_db_path();
        let a = sample_document();
        let mut b = a.clone();
        b.title = "Replacement".to_string();

        let mut db = Db::open(&path).unwrap();
        db.save_document(&a).unwrap();

        // Make every write on this connection fail (SQLITE_READONLY).
        db.conn.pragma_update(None, "query_only", true).unwrap();
        let result = db.save_document(&b);
        let err = match result {
            Err(err) => err,
            Ok(()) => panic!("save_document must fail while read-only"),
        };
        let message = err.to_string().to_lowercase();
        assert!(
            message.contains("readonly") || message.contains("read-only"),
            "expected a write failure, got: {err}"
        );
        db.conn.pragma_update(None, "query_only", false).unwrap();

        assert_eq!(db.load_document().unwrap(), a);

        // The aborted save must not leave a dangling transaction behind.
        db.save_document(&b).unwrap();
        assert_eq!(db.load_document().unwrap(), b);
    }

    #[test]
    fn open_sets_wal_and_user_version() {
        let path = temp_db_path();
        let db = Db::open(&path).unwrap();
        let mode: String = db
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        let version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn load_without_document_errors() {
        let path = temp_db_path();
        let db = Db::open(&path).unwrap();
        assert!(matches!(
            db.load_document(),
            Err(DbError::NoDocument)
        ));
    }

    /// A genuine failure (here: the table is gone) must not be reported as
    /// `NoDocument`, so callers do not mistake it for a first launch.
    #[test]
    fn load_failure_is_not_reported_as_no_document() {
        let path = temp_db_path();
        let db = Db::open(&path).unwrap();
        db.conn.execute_batch("DROP TABLE documents").unwrap();
        match db.load_document() {
            Err(DbError::Sqlite(_)) => {}
            other => panic!("expected DbError::Sqlite, got {other:?}"),
        }
    }

    #[test]
    fn open_rejects_newer_schema_version() {
        let path = temp_db_path();
        {
            let db = Db::open(&path).unwrap();
            db.conn.pragma_update(None, "user_version", 2_i64).unwrap();
        }

        let err = match Db::open(&path) {
            Ok(_) => panic!("expected DbError::SchemaTooNew, got a successful open"),
            Err(err) => err,
        };
        match err {
            err @ DbError::SchemaTooNew {
                found: 2,
                supported: 1,
            } => {
                assert!(err.to_string().contains("newer than this build"));
            }
            other => panic!("expected DbError::SchemaTooNew, got {other:?}"),
        }
    }

    #[test]
    fn open_creates_missing_file() {
        let path = temp_db_path();
        assert!(Path::new(&path).parent().unwrap().exists());
        assert!(!path.exists());
        let _db = Db::open(&path).unwrap();
        assert!(path.exists());
    }

    fn insert_op(offset: usize) -> Op {
        Op::InsertText {
            block_id: Uuid::new_v4(),
            offset,
            text: "x".to_string(),
        }
    }

    #[test]
    fn append_and_load_ops_round_trip_in_order() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();
        assert!(db.load_ops().unwrap().is_empty());

        let first = insert_op(0);
        let second = Op::SetTitle {
            old: "a".to_string(),
            new: "b".to_string(),
        };
        let seq1 = db.append_op(&first).unwrap();
        let seq2 = db.append_op(&second).unwrap();
        assert!(seq1 < seq2, "sequence numbers must increase: {seq1}, {seq2}");

        let log = db.load_ops().unwrap();
        assert_eq!(log, vec![(seq1, first), (seq2, second)]);
    }

    #[test]
    fn ops_survive_reopen() {
        let path = temp_db_path();
        let op = insert_op(3);
        {
            let mut db = Db::open(&path).unwrap();
            db.append_op(&op).unwrap();
        }
        let db = Db::open(&path).unwrap();
        let log = db.load_ops().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].1, op);
    }

    #[test]
    fn truncate_ops_after_drops_only_the_tail() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();
        let ops: Vec<Op> = (0..4).map(insert_op).collect();
        let seqs: Vec<i64> = ops.iter().map(|op| db.append_op(op).unwrap()).collect();

        db.truncate_ops_after(seqs[1]).unwrap();
        let log = db.load_ops().unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0], (seqs[0], ops[0].clone()));
        assert_eq!(log[1], (seqs[1], ops[1].clone()));

        // Truncating past the end is a no-op; truncating everything empties it.
        db.truncate_ops_after(seqs[3]).unwrap();
        assert_eq!(db.load_ops().unwrap().len(), 2);
        db.truncate_ops_after(0).unwrap();
        assert!(db.load_ops().unwrap().is_empty());

        // A truncate is durable and ids keep increasing after it.
        let next = db.append_op(&ops[0]).unwrap();
        assert!(next > seqs[3]);
    }
}
