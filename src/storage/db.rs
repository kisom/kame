//! SQLite-backed persistence for a single [`Document`].
//!
//! The storage layer keeps exactly one document: the whole [`Document`] is
//! serialized to JSON and stored in a single row of the `documents` table.
//! WAL journalling plus transactional writes mean an interrupted save leaves
//! the previously committed document intact.

use std::path::Path;

use rusqlite::{params, Connection};

use crate::model::Document;

pub type Error = Box<dyn std::error::Error>;

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
    pub fn open(path: &Path) -> Result<Db, Error> {
        let conn = Connection::open(path)?;

        // `PRAGMA journal_mode` returns the resulting mode as a row, so it has
        // to be read rather than executed.
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(format!("failed to enable WAL journal mode (got {mode:?})").into());
        }

        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        conn.execute_batch(SCHEMA)?;

        Ok(Db { conn })
    }

    /// Reads the stored document. Errors if no document has been saved yet.
    pub fn load_document(&self) -> Result<Document, Error> {
        let json: String = self
            .conn
            .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
                row.get(0)
            })
            .map_err(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => "no document stored".to_string(),
                other => other.to_string(),
            })?;
        let doc = serde_json::from_str(&json)?;
        Ok(doc)
    }

    /// Writes the document in a single transaction, replacing any prior one.
    pub fn save_document(&mut self, doc: &Document) -> Result<(), Error> {
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
        assert!(db.load_document().is_err());
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
