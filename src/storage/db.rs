//! SQLite-backed persistence for a single [`Document`].
//!
//! The storage layer keeps exactly one document: the whole [`Document`] is
//! serialized to JSON and stored in a single row of the `documents` table.
//! WAL journalling plus transactional writes mean an interrupted save leaves
//! the previously committed document intact.

use std::fmt;
use std::path::Path;

use rusqlite::{params, Connection};

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
const SCHEMA_VERSION: i64 = 1;

/// The whole document lives in the single row `id = 1`.
const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS documents (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
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
}
