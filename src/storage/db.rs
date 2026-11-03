//! SQLite-backed persistence for a single [`Document`].
//!
//! The storage layer keeps exactly one document: the whole [`Document`] is
//! serialized to JSON and stored in a single row of the `documents` table.
//! WAL journalling plus transactional writes mean an interrupted save leaves
//! the previously committed document intact.

use std::fmt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

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
    /// A stored meta value could not be parsed (e.g. a corrupt `undo_cursor`).
    ///
    /// Kept distinct from [`DbError::Sqlite`] because it is not a database
    /// failure: the row is there, its contents are nonsense.
    Meta(String),
    /// No snapshot with that id exists.
    ///
    /// Distinct from [`DbError::NoDocument`]: a fresh database is a legitimate
    /// first launch, whereas restoring a missing snapshot means the caller
    /// asked for something that is gone.
    SnapshotNotFound(i64),
}

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
            DbError::Meta(value) => write!(f, "unreadable meta value: {value}"),
            DbError::SnapshotNotFound(id) => write!(f, "no snapshot with id {id}"),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DbError::Sqlite(err) => Some(err),
            DbError::Json(err) => Some(err),
            DbError::NoDocument
            | DbError::SchemaTooNew { .. }
            | DbError::JournalMode(_)
            | DbError::Meta(_)
            | DbError::SnapshotNotFound(_) => None,
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
/// The `ops` and `snapshots` tables were added within version 1: they are
/// created with `CREATE TABLE IF NOT EXISTS` on every open, so a database
/// written by an older build gains them in place without a version bump or
/// migration.
const SCHEMA_VERSION: i64 = 1;

/// `metadata` holds what the document says about itself, as JSON in its single
/// row: apart from the head so that edits to it are not undo steps and restoring
/// a snapshot leaves it alone. A file from before it gains the table on open, and
/// the session fills the empty row with defaults.
///
/// The whole document lives in the single row `id = 1`; every edit that
/// produced it is appended to `ops`. `meta` holds the undo cursor: the number
/// of ops in the log that are applied to the head document (`0..=n`). Head and
/// cursor are always written in the same transaction, so they cannot disagree.
///
/// `snapshots` holds complete point-in-time copies of the document as JSON,
/// independent of both the head row and the op log: restoring one never
/// touches either, and deleting ops never shortens a snapshot.
const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS documents (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ops (
    seq  INTEGER PRIMARY KEY AUTOINCREMENT,
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS metadata (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS snapshots (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp INTEGER,
    label     TEXT,
    json      TEXT
)";

/// Key under which the applied-op count is stored in `meta`.
const UNDO_CURSOR_KEY: &str = "undo_cursor";

/// Metadata for one stored snapshot: enough to list snapshots without loading
/// their (potentially large) document JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotMeta {
    /// Row id; pass it to [`Db::restore_snapshot`].
    pub id: i64,
    /// Unix timestamp (seconds) of when the snapshot was taken.
    pub timestamp: i64,
    /// Optional human-readable label, e.g. a version name.
    pub label: Option<String>,
}

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
        Self::init(conn)
    }

    /// Opens a fresh, private in-memory database for a session that must not
    /// touch the disk — e.g. a failed open that still wants to show the author
    /// an editable document and its error, without clobbering the bad file.
    pub fn open_in_memory() -> Result<Db, DbError> {
        Self::init(Connection::open_in_memory()?)
    }

    /// Applies WAL journalling and the schema to an opened connection.
    fn init(conn: Connection) -> Result<Db, DbError> {
        // A second kame instance opening the same document contends for the
        // WAL write lock; retry briefly instead of failing the open outright.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;

        // `PRAGMA journal_mode` returns the resulting mode as a row, so it has
        // to be read rather than executed. An in-memory database reports
        // "memory" instead (it has nothing to journal to disk), which is fine.
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") && !mode.eq_ignore_ascii_case("memory") {
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

    /// Writes a complete copy of the database to `path`, which must not exist:
    /// the head, the op log, snapshots and metadata alike.
    pub fn save_copy(&self, path: &Path) -> Result<(), DbError> {
        let target = path
            .to_str()
            .ok_or_else(|| rusqlite::Error::InvalidPath(path.to_path_buf()))?;
        self.conn.execute("VACUUM INTO ?1", [target])?;
        Ok(())
    }

    /// Reads the document's metadata, or `None` if it has none stored yet.
    pub fn load_metadata(&self) -> Result<Option<crate::model::Metadata>, DbError> {
        let json = self
            .conn
            .query_row("SELECT json FROM metadata WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            });
        match json {
            Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// Replaces the document's metadata.
    pub fn save_metadata(&mut self, metadata: &crate::model::Metadata) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO metadata (id, json) VALUES (1, ?1)",
            [serde_json::to_string(metadata)?],
        )?;
        Ok(())
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
    #[cfg(test)]
    pub fn append_op(&mut self, op: &Op) -> Result<i64, DbError> {
        let json = serde_json::to_string(op)?;
        self.conn
            .execute("INSERT INTO ops (json) VALUES (?1)", params![json])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Reads the whole op log in application order: `(seq, op)`, oldest first.
    pub fn load_ops(&self) -> Result<Vec<(i64, Op)>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, json FROM ops ORDER BY seq")?;
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
    /// A new edit after an undo discards the redo tail so the log stays a
    /// single linear history. [`Db::record_edit`] does this itself inside the
    /// same transaction; call this directly only for a standalone truncation.
    #[cfg(test)]
    pub fn truncate_ops_after(&mut self, seq: i64) -> Result<(), DbError> {
        self.conn
            .execute("DELETE FROM ops WHERE seq > ?1", params![seq])?;
        Ok(())
    }

    /// The stored undo cursor: how many ops of the log are applied to the head
    /// document. `0` when nothing has recorded one yet (fresh database).
    pub fn cursor(&self) -> Result<usize, DbError> {
        read_cursor(&self.conn)
    }

    /// Persists one edit atomically: appends `op`, replaces the head document
    /// with `doc`, drops any redo tail, and sets the cursor to
    /// `current + 1` — all in one transaction.
    ///
    /// This is the write path for `Editor::apply_edit`. Because head, log and
    /// cursor move together, a crash can never leave a head document that
    /// disagrees with its history (which is what makes a second undo of the
    /// same op impossible).
    ///
    /// Returns the sequence number of the appended op.
    pub fn record_edit(&mut self, doc: &Document, op: &Op) -> Result<i64, DbError> {
        let json = serde_json::to_string(doc)?;
        let op_json = serde_json::to_string(op)?;
        let tx = self.conn.transaction()?;
        let cursor = read_cursor(&tx)?;
        truncate_ops_to_cursor(&tx, cursor)?;
        tx.execute("INSERT INTO ops (json) VALUES (?1)", params![op_json])?;
        let seq = tx.last_insert_rowid();
        tx.execute(
            "INSERT OR REPLACE INTO documents (id, json) VALUES (1, ?1)",
            params![json],
        )?;
        write_cursor(&tx, cursor + 1)?;
        tx.commit()?;
        Ok(seq)
    }

    /// Persists a head document and its cursor atomically, changing no ops.
    ///
    /// The write path for undo/redo: the op log is untouched, the cursor moves,
    /// and the head follows it in the same transaction.
    pub fn save_state(&mut self, doc: &Document, cursor: usize) -> Result<(), DbError> {
        let json = serde_json::to_string(doc)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO documents (id, json) VALUES (1, ?1)",
            params![json],
        )?;
        write_cursor(&tx, cursor)?;
        tx.commit()?;
        Ok(())
    }

    /// Replaces the head document with `doc`, resets the cursor to `0` and
    /// drops the whole op log — all in one transaction.
    ///
    /// The write path for restoring a snapshot. Undo/redo need the log to
    /// survive, but a restore replaces the text the log describes, so the log
    /// is discarded with the cursor reset instead of truncated to it: a
    /// stale op must never be left for a reopen to offer as redo against a
    /// document it never touched. Doing all three writes together means a
    /// crash or I/O error can never split the restored text from an intact
    /// pre-restore log.
    pub fn restore_state(&mut self, doc: &Document) -> Result<(), DbError> {
        let json = serde_json::to_string(doc)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO documents (id, json) VALUES (1, ?1)",
            params![json],
        )?;
        tx.execute("DELETE FROM ops", [])?;
        write_cursor(&tx, 0)?;
        tx.commit()?;
        Ok(())
    }

    /// Stores a complete copy of `doc` as a point-in-time snapshot and returns
    /// its id.
    ///
    /// The snapshot is independent of the head document and the op log: later
    /// saves neither change it nor are changed by it, and it survives undo,
    /// redo and op-log truncation. `doc` is snapshotted as given, so callers
    /// capture the version they actually mean rather than whatever is stored.
    pub fn create_snapshot(&mut self, doc: &Document, label: Option<&str>) -> Result<i64, DbError> {
        let json = serde_json::to_string(doc)?;
        self.conn.execute(
            "INSERT INTO snapshots (timestamp, label, json) VALUES (?1, ?2, ?3)",
            params![now_unix(), label, json],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Lists stored snapshots newest first, without loading their documents.
    pub fn list_snapshots(&self) -> Result<Vec<SnapshotMeta>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, label FROM snapshots ORDER BY timestamp DESC, id DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SnapshotMeta {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                label: row.get(2)?,
            })
        })?;
        let mut snapshots = Vec::new();
        for row in rows {
            snapshots.push(row?);
        }
        Ok(snapshots)
    }

    /// Reads snapshot `id` back as a [`Document`].
    ///
    /// This is a pure read: the head document and op log are not modified, so
    /// restoring is `create_snapshot`'s inverse only once the caller saves the
    /// returned document (e.g. via [`Db::save_document`] or
    /// [`Db::save_state`]).
    ///
    /// Returns [`DbError::SnapshotNotFound`] when no such snapshot exists.
    pub fn restore_snapshot(&self, id: i64) -> Result<Document, DbError> {
        let json: String = self
            .conn
            .query_row(
                "SELECT json FROM snapshots WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .map_err(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => DbError::SnapshotNotFound(id),
                other => DbError::Sqlite(other),
            })?;
        let doc = serde_json::from_str(&json)?;
        Ok(doc)
    }
}

/// Current time as a Unix timestamp in whole seconds.
///
/// A clock before the epoch is treated as the epoch rather than an error:
/// snapshots stay ordered by row id in that case (see [`Db::list_snapshots`]).
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Reads the undo cursor, defaulting to `0` when no row has been written.
fn read_cursor(conn: &Connection) -> Result<usize, DbError> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![UNDO_CURSOR_KEY],
            |row| row.get(0),
        )
        .optional()?;
    match stored {
        None => Ok(0),
        // A negative or non-numeric cursor is corruption, not a fresh database:
        // silently resetting it to 0 would resurrect undone ops.
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| DbError::Meta(format!("{UNDO_CURSOR_KEY} = {value:?}"))),
    }
}

fn write_cursor(conn: &Connection, cursor: usize) -> Result<(), DbError> {
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![UNDO_CURSOR_KEY, cursor.to_string()],
    )?;
    Ok(())
}

/// Drops every op past the first `cursor` (in `seq` order), the redo tail.
fn truncate_ops_to_cursor(conn: &Connection, cursor: usize) -> Result<(), DbError> {
    let keep: Option<i64> = conn
        .query_row(
            "SELECT seq FROM ops ORDER BY seq LIMIT 1 OFFSET ?1",
            params![cursor as i64],
            |row| row.get(0),
        )
        .optional()?;
    match keep {
        // The op at `cursor` is the first unapplied one: everything from it on
        // is redo tail.
        Some(seq) => conn.execute("DELETE FROM ops WHERE seq >= ?1", params![seq])?,
        // No op at that index: the tail is already empty.
        None => 0,
    };
    Ok(())
}

impl Drop for Db {
    fn drop(&mut self) {
        // Checkpoint and truncate the WAL so a clean exit leaves only the
        // .kame file. A crash or kill leaves -wal/-shm behind — that is normal
        // and harmless: SQLite replays the WAL on the next open. Here we shrink
        // the WAL, and the connection's own close removes both sidecars.
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
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
    fn dropping_the_connection_removes_wal_and_shm_sidecars() {
        let path = temp_db_path();
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));

        {
            let mut db = Db::open(&path).unwrap();
            db.save_document(&sample_document()).unwrap();
            // WAL journalling creates both sidecars while the connection is
            // open.
            assert!(wal.exists(), "expected {} while open", wal.display());
        }

        // A clean close checkpoints the WAL and removes both sidecars, leaving
        // only the database file.
        assert!(
            !wal.exists(),
            "wal survived a clean close: {}",
            wal.display()
        );
        assert!(
            !shm.exists(),
            "shm survived a clean close: {}",
            shm.display()
        );
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
        assert!(matches!(db.load_document(), Err(DbError::NoDocument)));
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
        assert!(
            seq1 < seq2,
            "sequence numbers must increase: {seq1}, {seq2}"
        );

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

    #[test]
    fn cursor_defaults_to_zero_and_round_trips() {
        let path = temp_db_path();
        let db = Db::open(&path).unwrap();
        assert_eq!(db.cursor().unwrap(), 0, "fresh database has no cursor row");

        // The meta table exists and holds a single row after a write.
        db.conn
            .execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('undo_cursor', '3')",
                [],
            )
            .unwrap();
        assert_eq!(db.cursor().unwrap(), 3);
    }

    #[test]
    fn cursor_survives_reopen() {
        let path = temp_db_path();
        {
            let mut db = Db::open(&path).unwrap();
            db.save_state(&sample_document(), 4).unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.cursor().unwrap(), 4);
    }

    /// A corrupt cursor must be reported, not silently reset to 0 — resetting
    /// would resurrect ops the user had undone.
    #[test]
    fn corrupt_cursor_is_an_error() {
        let path = temp_db_path();
        let db = Db::open(&path).unwrap();
        db.conn
            .execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('undo_cursor', 'nonsense')",
                [],
            )
            .unwrap();
        match db.cursor() {
            Err(DbError::Meta(_)) => {}
            other => panic!("expected DbError::Meta, got {other:?}"),
        }
    }

    #[test]
    fn record_edit_writes_op_head_and_cursor_together() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();

        let mut doc = sample_document();
        let op = insert_op(0);
        let seq = db.record_edit(&doc, &op).unwrap();
        assert_eq!(db.cursor().unwrap(), 1);
        assert_eq!(db.load_document().unwrap(), doc);
        assert_eq!(db.load_ops().unwrap(), vec![(seq, op.clone())]);

        doc.title = "Second".to_string();
        let seq2 = db.record_edit(&doc, &op).unwrap();
        assert_eq!(db.cursor().unwrap(), 2);
        assert_eq!(db.load_ops().unwrap().len(), 2);
        assert!(seq2 > seq);
    }

    #[test]
    fn save_state_moves_head_and_cursor_without_touching_the_log() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();
        let doc = sample_document();
        let op = insert_op(1);
        db.record_edit(&doc, &op).unwrap();

        let mut undone = doc.clone();
        undone.title = "Undone".to_string();
        db.save_state(&undone, 0).unwrap();

        assert_eq!(db.cursor().unwrap(), 0);
        assert_eq!(db.load_document().unwrap(), undone);
        assert_eq!(db.load_ops().unwrap().len(), 1, "ops are untouched by undo");
    }

    /// `record_edit` after an undo replaces the redo tail inside the same
    /// transaction, so the log can never grow a stale branch.
    #[test]
    fn record_edit_after_undo_truncates_the_redo_tail() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();
        let doc = sample_document();

        for offset in 0..3 {
            db.record_edit(&doc, &insert_op(offset)).unwrap();
        }
        assert_eq!(db.load_ops().unwrap().len(), 3);

        // Undo two: cursor 1.
        db.save_state(&doc, 1).unwrap();
        let replacement = insert_op(9);
        db.record_edit(&doc, &replacement).unwrap();

        let log = db.load_ops().unwrap();
        assert_eq!(log.len(), 2, "the two undone ops are gone");
        assert_eq!(log[1].1, replacement);
        assert_eq!(db.cursor().unwrap(), 2);

        // Undoing everything and editing again clears the log outright.
        db.save_state(&doc, 0).unwrap();
        db.record_edit(&doc, &replacement).unwrap();
        let log = db.load_ops().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].1, replacement);
        assert_eq!(db.cursor().unwrap(), 1);
    }

    /// `restore_state` must land head, cursor and an empty log together, so a
    /// reopen can never pair the restored text with the pre-restore history.
    #[test]
    fn restore_state_replaces_head_resets_cursor_and_clears_the_log() {
        let path = temp_db_path();
        let restored = sample_document();
        let mut edited = restored.clone();
        edited.title = "Edited after the snapshot".to_string();
        edited.blocks[0].text = "text that must disappear".to_string();

        {
            let mut db = Db::open(&path).unwrap();
            db.save_document(&restored).unwrap();
            let id = db.create_snapshot(&restored, Some("kept")).unwrap();
            for offset in 0..3 {
                db.record_edit(&edited, &insert_op(offset)).unwrap();
            }
            assert_eq!(db.cursor().unwrap(), 3);

            // The caller reads the snapshot back, then writes the restore.
            let doc = db.restore_snapshot(id).unwrap();
            db.restore_state(&doc).unwrap();

            assert_eq!(db.load_document().unwrap(), restored);
            assert_eq!(db.cursor().unwrap(), 0);
            assert!(
                db.load_ops().unwrap().is_empty(),
                "the log describing the replaced text is gone"
            );
            assert_eq!(db.list_snapshots().unwrap().len(), 1, "snapshots survive");
        }

        // A reopen sees the same three facts: nothing for `Editor` to put on
        // the redo stack, and a cursor that matches the empty log.
        let mut db = Db::open(&path).unwrap();
        assert_eq!(db.load_document().unwrap(), restored);
        assert_eq!(db.cursor().unwrap(), 0);
        assert!(db.load_ops().unwrap().is_empty());
        // New edits start a clean log at cursor 1.
        db.record_edit(&restored, &insert_op(0)).unwrap();
        assert_eq!(db.cursor().unwrap(), 1);
        assert_eq!(db.load_ops().unwrap().len(), 1);
    }

    /// A failed `restore_state` must leave the pre-restore head, cursor and log
    /// untouched — never a restored document beside a stale history.
    #[test]
    fn failed_restore_state_changes_nothing() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();
        let original = sample_document();
        let mut edited = original.clone();
        edited.title = "Edited".to_string();
        db.record_edit(&edited, &insert_op(0)).unwrap();

        db.conn.pragma_update(None, "query_only", true).unwrap();
        assert!(
            db.restore_state(&original).is_err(),
            "the write must fail while read-only"
        );
        db.conn.pragma_update(None, "query_only", false).unwrap();

        assert_eq!(db.load_document().unwrap(), edited);
        assert_eq!(db.cursor().unwrap(), 1);
        assert_eq!(db.load_ops().unwrap().len(), 1);
        // The aborted transaction must not be left dangling.
        db.restore_state(&original).unwrap();
        assert_eq!(db.load_document().unwrap(), original);
        assert_eq!(db.cursor().unwrap(), 0);
        assert!(db.load_ops().unwrap().is_empty());
    }

    /// A snapshot is a point-in-time copy: restoring it yields the document
    /// exactly as it was when the snapshot was taken, no matter what was saved
    /// afterwards.
    #[test]
    fn snapshot_then_edit_then_restore() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();
        let doc_v1 = sample_document();
        let mut doc_v2 = doc_v1.clone();
        doc_v2.title = "Edited".to_string();
        doc_v2.blocks[0].text = "hello again".to_string();

        db.save_document(&doc_v1).unwrap();
        let id = db.create_snapshot(&doc_v1, Some("v1")).unwrap();
        db.save_document(&doc_v2).unwrap();
        assert_eq!(db.load_document().unwrap(), doc_v2);

        assert_eq!(db.restore_snapshot(id).unwrap(), doc_v1);
        // Restoring is a read: the head document only changes when the caller
        // saves the restored copy.
        assert_eq!(db.load_document().unwrap(), doc_v2);
    }

    #[test]
    fn list_snapshots_returns_meta() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();
        let before = now_unix();
        let mut doc = sample_document();
        let first = db.create_snapshot(&doc, Some("v1")).unwrap();
        doc.title = "Second".to_string();
        let second = db.create_snapshot(&doc, None).unwrap();
        let after = now_unix();

        let snaps = db.list_snapshots().unwrap();
        assert_eq!(snaps.len(), 2);
        // Newest first.
        assert_eq!(snaps[0].id, second);
        assert_eq!(snaps[0].label, None);
        assert_eq!(snaps[1].id, first);
        assert_eq!(snaps[1].label.as_deref(), Some("v1"));
        for snap in &snaps {
            assert!(
                snap.timestamp >= before && snap.timestamp <= after,
                "timestamp {} outside [{before}, {after}]",
                snap.timestamp
            );
        }
    }

    #[test]
    fn snapshots_survive_reopen() {
        let path = temp_db_path();
        let doc = sample_document();
        let id = {
            let mut db = Db::open(&path).unwrap();
            db.save_document(&doc).unwrap();
            db.create_snapshot(&doc, Some("before restart")).unwrap()
        };

        let db = Db::open(&path).unwrap();
        let snaps = db.list_snapshots().unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].id, id);
        assert_eq!(snaps[0].label.as_deref(), Some("before restart"));
        assert_eq!(db.restore_snapshot(id).unwrap(), doc);
    }

    #[test]
    fn list_snapshots_is_empty_on_fresh_database() {
        let path = temp_db_path();
        let db = Db::open(&path).unwrap();
        assert!(db.list_snapshots().unwrap().is_empty());
    }

    /// Restoring an id that does not exist is reported as such, not as a
    /// corrupt document.
    #[test]
    fn restore_unknown_snapshot_errors() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();
        db.save_document(&sample_document()).unwrap();
        let id = db.create_snapshot(&sample_document(), None).unwrap();

        match db.restore_snapshot(id + 1) {
            Err(DbError::SnapshotNotFound(missing)) => assert_eq!(missing, id + 1),
            other => panic!("expected DbError::SnapshotNotFound, got {other:?}"),
        }
    }

    /// A snapshot stays independent of the head document: further saves change
    /// nothing that was already captured, and each snapshot is its own row.
    #[test]
    fn snapshots_are_independent_of_later_saves() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();
        let mut doc = sample_document();
        let original = doc.clone();
        let first = db.create_snapshot(&doc, None).unwrap();
        for i in 0..3 {
            doc.title = format!("rev {i}");
            db.save_document(&doc).unwrap();
        }
        assert_eq!(db.restore_snapshot(first).unwrap(), original);
    }

    /// Atomicity: a failed `record_edit` must leave head, log and cursor all
    /// unchanged, so none of them can disagree afterwards.
    #[test]
    fn failed_record_edit_changes_nothing() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();
        let original = sample_document();
        let op = insert_op(0);
        db.record_edit(&original, &op).unwrap();

        let mut changed = original.clone();
        changed.title = "Should not land".to_string();
        db.conn.pragma_update(None, "query_only", true).unwrap();
        let result = db.record_edit(&changed, &insert_op(5));
        assert!(result.is_err(), "write must fail while read-only");
        db.conn.pragma_update(None, "query_only", false).unwrap();

        assert_eq!(db.load_document().unwrap(), original);
        assert_eq!(db.load_ops().unwrap().len(), 1);
        assert_eq!(db.cursor().unwrap(), 1);
    }

    #[test]
    fn metadata_starts_absent_and_round_trips() {
        let mut db = Db::open_in_memory().unwrap();
        assert_eq!(db.load_metadata().unwrap(), None);
        let metadata = crate::model::Metadata {
            title: "A Title".to_string(),
            author: "K".to_string(),
            created: "2026-11-02".to_string(),
            published: String::new(),
            tags: vec!["lisp".to_string(), "hacks".to_string()],
        };
        db.save_metadata(&metadata).unwrap();
        assert_eq!(db.load_metadata().unwrap(), Some(metadata.clone()));
        let newer = crate::model::Metadata {
            author: "L".to_string(),
            ..metadata
        };
        db.save_metadata(&newer).unwrap();
        assert_eq!(db.load_metadata().unwrap(), Some(newer));
    }

    #[test]
    fn a_file_without_the_metadata_table_gains_it_on_open() {
        let dir = std::env::temp_dir().join(format!("kame-meta-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.kame");
        drop(Db::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE metadata").ok();
        drop(conn);
        let db = Db::open(&path).unwrap();
        assert_eq!(db.load_metadata().unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
