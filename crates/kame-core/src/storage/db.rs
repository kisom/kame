//! SQLite-backed persistence for a single [`Document`].
//!
//! The storage layer keeps exactly one document: the whole [`Document`] is
//! serialized to JSON and stored in a single row of the `documents` table.
//! WAL journalling plus transactional writes mean an interrupted save leaves
//! the previously committed document intact.

use std::fmt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::view;
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
    /// It holds something from a newer version of kame, which reading it here would leave out.
    Newer,
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
            DbError::Newer => f.write_str(
                "it has changes from a newer version of kame, so it can't be used here \
                 until kame is updated",
            ),
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
            | DbError::SnapshotNotFound(_)
            | DbError::Newer => None,
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
const POSITION_KEY: &str = "position";
const SYNC_KEY: &str = "sync";
const SYNC_PLACE_KEY: &str = "sync_place";

/// Where the author was in a document, kept in its file so that each document
/// reopens where it was left.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub block_id: Uuid,
    pub offset: usize,
    pub scroll: f32,
}

/// A failure making a private copy to read, as the error kame reports for a bad open.
fn scratch_error(err: std::io::Error) -> DbError {
    DbError::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
        Some(format!("could not copy the document to read it: {err}")),
    ))
}

/// `path` as a URI's path, with whatever a URI would read differently escaped.
fn uri_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for byte in path.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

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
    /// A private copy a read-only open is reading from. After `conn`, so that it goes after
    /// the connection using it has closed. Only ever held, for its `Drop`.
    #[allow(dead_code)]
    scratch: Option<ScratchCopy>,
}

/// A directory removed when it goes out of use.
struct ScratchCopy(std::path::PathBuf);

impl Drop for ScratchCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Db {
    /// Opens (creating if absent) the database at `path`, enables WAL
    /// journalling, and applies the schema for schema version 1.
    ///
    /// Fails if the database was created by a newer schema version, rather
    /// than silently stamping it back down to [`SCHEMA_VERSION`].
    /// Opens an existing file for reading only, for publishing what is written without
    /// any chance of changing it: no migration runs and nothing can be saved. An older
    /// file that needs migrating fails on whichever read needs what it lacks.
    ///
    /// Reading a WAL database ordinarily creates `-wal` and `-shm` files beside it and leaves
    /// them there, which matters in a synced folder. With no `-wal` there, nothing is
    /// waiting to be read from one, so the file is opened as immutable, which creates
    /// neither. With a `-wal` and a `-shm`, a writer has it open and may have newer text, so
    /// it is read as usual, adding nothing. A `-wal` alone is left by a crash or a sync, and
    /// reading that in place would create a `-shm`, so a private copy is read instead.
    ///
    /// A file from a newer build is refused here too, since it could be misread.
    pub fn open_read_only(path: &Path) -> Result<Db, DbError> {
        let db = Self::open_read_only_unchecked(path)?;
        let version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(DbError::SchemaTooNew {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        Ok(db)
    }

    fn open_read_only_unchecked(path: &Path) -> Result<Db, DbError> {
        let sibling = |suffix: &str| {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            std::path::PathBuf::from(name)
        };
        let wal = sibling("-wal");
        if wal.exists() && !sibling("-shm").exists() {
            let scratch = ScratchCopy(
                std::env::temp_dir().join(format!("kame-read-{}", uuid::Uuid::new_v4())),
            );
            std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
                .create(&scratch.0)
                .map_err(scratch_error)?;
            let copy = scratch.0.join("document.kame");
            std::fs::copy(path, &copy).map_err(scratch_error)?;
            std::fs::copy(&wal, scratch.0.join("document.kame-wal")).map_err(scratch_error)?;
            let conn = Connection::open_with_flags(&copy, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            conn.busy_timeout(std::time::Duration::from_secs(5))?;
            return Ok(Db {
                conn,
                scratch: Some(scratch),
            });
        }
        let conn = if wal.exists() {
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?
        } else {
            if !path.is_file() {
                // An immutable open reports a missing file only when it first reads.
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                    Some(format!("no file at {}", path.display())),
                )
                .into());
            }
            let uri = format!("file:{}?immutable=1", uri_path(path));
            Connection::open_with_flags(
                uri,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )?
        };
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Db {
            conn,
            scratch: None,
        })
    }

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

        Ok(Db {
            conn,
            scratch: None,
        })
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

    /// Puts everything in the `-wal` into the file itself and closes it, for a file about to be
    /// renamed, which would leave its `-wal` behind. Fails if any of it couldn't be put there.
    pub fn close(self) -> Result<(), DbError> {
        let busy: i64 = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        if busy != 0 {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("the log could not all be put into the file".to_string()),
            )
            .into());
        }
        Ok(())
    }

    /// How long to wait for another connection before giving up, shortened by tests that wait on
    /// purpose.
    #[cfg(test)]
    pub(crate) fn set_busy_timeout(&self, timeout: std::time::Duration) -> Result<(), DbError> {
        self.conn.busy_timeout(timeout)?;
        Ok(())
    }

    /// Where the author last was in this document, if that was recorded and is
    /// still readable. It is a convenience, so a bad value is the same as none.
    pub fn load_position(&self) -> Option<Position> {
        let json: String = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![POSITION_KEY],
                |row| row.get(0),
            )
            .ok()?;
        serde_json::from_str(&json).ok()
    }

    /// Records where the author is. This is not an edit: it has no op and leaves the
    /// undo cursor alone.
    pub fn save_position(&mut self, position: &Position) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![POSITION_KEY, serde_json::to_string(position)?],
        )?;
        Ok(())
    }

    /// What a sync tool last recorded in this file, opaque to the storage layer. Like the
    /// position, this isn't an edit, so it has no op.
    pub fn load_sync_state(&self) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![SYNC_KEY],
                |row| row.get(0),
            )
            .ok()
    }

    pub fn save_sync_state(&mut self, state: &str) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![SYNC_KEY, state],
        )?;
        Ok(())
    }

    /// Where a sync tool last synced this file, opaque to the storage layer like the state beside
    /// it. A file last synced by a build that didn't record this has none.
    pub fn load_sync_place(&self) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![SYNC_PLACE_KEY],
                |row| row.get(0),
            )
            .ok()
    }

    pub fn save_sync_place(&mut self, place: &str) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![SYNC_PLACE_KEY, place],
        )?;
        Ok(())
    }

    /// A sync tool's state and place, saved together or not at all. A file with one and not the
    /// other would say it had synced somewhere it hadn't.
    pub fn save_sync(&mut self, state: &str, place: &str) -> Result<(), DbError> {
        let tx = self.conn.transaction()?;
        for (key, value) in [(SYNC_KEY, state), (SYNC_PLACE_KEY, place)] {
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Forgets what a sync tool recorded, for a file that is not the one it recorded it for.
    pub fn clear_sync_state(&mut self) -> Result<(), DbError> {
        self.conn.execute(
            "DELETE FROM meta WHERE key IN (?1, ?2)",
            params![SYNC_KEY, SYNC_PLACE_KEY],
        )?;
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

    /// Whether this file holds something from a newer version of kame: a field this build would
    /// drop by saving, a kind, style or other value it doesn't know in the head, or a step of
    /// history it can't read or would lose a field of by undoing, redoing or copying it.
    pub fn holds_newer(&self) -> bool {
        if self.leaves_out_newer() {
            return true;
        }
        let Ok(mut statement) = self.conn.prepare("SELECT json FROM ops") else {
            return false;
        };
        let rows: Vec<String> = match statement.query_map([], |row| row.get::<_, String>(0)) {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => return false,
        };
        rows.iter().any(|json| {
            drops_something::<Op>(json)
                || serde_json::from_str::<Op>(json)
                    .is_err_and(|err| err.to_string().contains("unknown variant"))
        })
    }

    /// Reads the stored document. Errors if no document has been saved yet.
    ///
    /// Only `QueryReturnedNoRows` becomes [`DbError::NoDocument`]; every other
    /// failure (missing table, I/O, locking) is propagated unchanged.
    ///
    /// Its spans and anchors are made to fit its text ([`Document::fit`]), since a
    /// damaged file's needn't. That's only in memory, and the file keeps what it had until the
    /// head is next written.
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
        let mut doc: Document = serde_json::from_str(&json)?;
        doc.fit();
        Ok(doc)
    }

    /// What this build can show of a head it can't read only because it holds something from a
    /// newer version of kame, made to fit as the head is. `None` when there's no such head: one
    /// this build reads whole, a damaged one, or none. It leaves out what it doesn't know, so it's
    /// never to be written back.
    pub fn load_view(&self) -> Result<Option<Document>, DbError> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(json.and_then(|json| view::view_of(&json)).map(|mut doc| {
            doc.fit();
            doc
        }))
    }

    /// The stored document for whatever reads it and never writes it back, such as a site build
    /// or a notebook's lists and search. It's [`Db::load_document`], or, for a head holding a kind,
    /// style or value only a newer version of kame knows, what this build can read of it: what it
    /// doesn't know is left out, with nothing standing in its place, and so are the notes on a
    /// block it doesn't know. [`Db::leaves_out_newer`] says whether anything was. Damage is still
    /// an error.
    pub fn load_readable(&self) -> Result<Document, DbError> {
        match self.load_document() {
            Err(DbError::Json(err)) => {
                let json: Option<String> = self
                    .conn
                    .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
                        row.get(0)
                    })
                    .optional()?;
                match json.and_then(|json| view::readable_of(&json)) {
                    Some(mut doc) => {
                        doc.fit();
                        Ok(doc)
                    }
                    None => Err(DbError::Json(err)),
                }
            }
            read => read,
        }
    }

    /// Whether reading this file's document and properties here leaves out something a newer
    /// version of kame wrote into them: a field, or a kind, style or other value this build doesn't
    /// know. Unlike [`Db::holds_newer`], its history isn't looked at, since no reader shows it.
    pub fn leaves_out_newer(&self) -> bool {
        let row =
            |sql: &str| -> Option<String> { self.conn.query_row(sql, [], |row| row.get(0)).ok() };
        row("SELECT json FROM documents WHERE id = 1").is_some_and(|json| view::is_newer(&json))
            || row("SELECT json FROM metadata WHERE id = 1").is_some_and(|json| {
                drops_something_held_at::<crate::model::Metadata>(Some("metadata"), &json)
            })
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

    /// Reads the whole op log in application order: `(seq, op)`, oldest first. A document or block
    /// an op carries is made to fit as the head is ([`Op::fit`]).
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
            let mut op: Op = serde_json::from_str(&json)?;
            op.fit();
            ops.push((seq, op));
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

    /// Persists an edit that joined up with the last one: the last entry in the log becomes
    /// `op`, and the head follows it in one transaction, like [`Db::record_edit`].
    pub fn replace_last_edit(&mut self, doc: &Document, op: &Op) -> Result<(), DbError> {
        let json = serde_json::to_string(doc)?;
        let op_json = serde_json::to_string(op)?;
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE ops SET json = ?1 WHERE seq = (SELECT MAX(seq) FROM ops)",
            params![op_json],
        )?;
        if changed == 0 {
            tx.execute("INSERT INTO ops (json) VALUES (?1)", params![op_json])?;
            let cursor = read_cursor(&tx)?;
            write_cursor(&tx, cursor + 1)?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO documents (id, json) VALUES (1, ?1)",
            params![json],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Replaces the whole op log and the cursor in one transaction, then gives the space back
    /// to the disk, since a smaller file is why this is done.
    pub fn replace_log(&mut self, ops: &[Op], cursor: usize) -> Result<(), DbError> {
        let rows = ops
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<String>, _>>()?;
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM ops", [])?;
        for row in &rows {
            tx.execute("INSERT INTO ops (json) VALUES (?1)", params![row])?;
        }
        write_cursor(&tx, cursor)?;
        tx.commit()?;
        self.conn
            .execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    /// Replaces the head document, the whole op log, the cursor and the metadata, and the document
    /// in each of `snapshots` by id, in one transaction, for a history written whole, so that a
    /// crash leaves all of it as it was or all of it new. A snapshot keeps its id, label and time,
    /// and one that isn't there fails the whole of it.
    pub fn replace_history(
        &mut self,
        doc: &Document,
        ops: &[Op],
        cursor: usize,
        metadata: &crate::model::Metadata,
        snapshots: &[(i64, Document)],
    ) -> Result<(), DbError> {
        let json = serde_json::to_string(doc)?;
        let metadata = serde_json::to_string(metadata)?;
        let rows = ops
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<String>, _>>()?;
        let snapshots = snapshots
            .iter()
            .map(|(id, doc)| Ok((*id, serde_json::to_string(doc)?)))
            .collect::<Result<Vec<(i64, String)>, DbError>>()?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO documents (id, json) VALUES (1, ?1)",
            params![json],
        )?;
        tx.execute("DELETE FROM ops", [])?;
        for row in &rows {
            tx.execute("INSERT INTO ops (json) VALUES (?1)", params![row])?;
        }
        write_cursor(&tx, cursor)?;
        tx.execute(
            "INSERT OR REPLACE INTO metadata (id, json) VALUES (1, ?1)",
            [metadata],
        )?;
        for (id, json) in &snapshots {
            let changed = tx.execute(
                "UPDATE snapshots SET json = ?2 WHERE id = ?1",
                params![id, json],
            )?;
            if changed == 0 {
                return Err(DbError::SnapshotNotFound(*id));
            }
        }
        tx.commit()?;
        Ok(())
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

    /// Persists an edit taken out of the history: the head becomes `doc`, the cursor becomes
    /// `cursor`, and the log loses everything past it, all in one transaction, so that a crash
    /// can't leave an op for a redo to bring back.
    pub fn forget_last_edit(&mut self, doc: &Document, cursor: usize) -> Result<(), DbError> {
        let json = serde_json::to_string(doc)?;
        let tx = self.conn.transaction()?;
        truncate_ops_to_cursor(&tx, cursor)?;
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

    /// Every snapshot this build can read whole, by id, oldest first, for saying something in each
    /// of them again. One that won't parse, or holds a field from a newer version that writing it
    /// again would lose, is left out, so that it's kept as it was.
    pub fn snapshot_documents(&self) -> Result<Vec<(i64, Document)>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, json FROM snapshots ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut snapshots = Vec::new();
        for row in rows {
            let (id, json) = row?;
            if drops_something::<Document>(&json) {
                continue;
            }
            if let Ok(doc) = serde_json::from_str(&json) {
                snapshots.push((id, doc));
            }
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
    /// Returns [`DbError::SnapshotNotFound`] when no such snapshot exists, and [`DbError::Newer`]
    /// for one holding something from a newer version of kame, which restoring here would lose.
    /// The document is made to fit as the head is.
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
        if view::is_newer(&json) {
            return Err(DbError::Newer);
        }
        let mut doc: Document = serde_json::from_str(&json)?;
        doc.fit();
        Ok(doc)
    }
}

/// Whether reading `json` as a `T` and writing it again would lose something from a newer version:
/// a field, or a level or depth past what this build's model has, which it would fit. JSON that
/// isn't a `T` at all loses nothing this way.
fn drops_something<T: Serialize + serde::de::DeserializeOwned>(json: &str) -> bool {
    drops_something_held_at::<T>(None, json)
}

/// [`drops_something`], for a `T` that's elsewhere written under the key `held_at`, as metadata is
/// under `metadata` in a revision, so that a field it leaves out at its default is known here too.
fn drops_something_held_at<T: Serialize + serde::de::DeserializeOwned>(
    held_at: Option<&str>,
    json: &str,
) -> bool {
    let Ok(written) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    let Ok(read) = serde_json::from_str::<T>(json) else {
        return false;
    };
    crate::model::holds_newer_values(&written)
        || serde_json::to_value(read).is_ok_and(|known| match held_at {
            Some(key) => crate::model::has_unknown_fields_held_at(key, &written, &known),
            None => crate::model::has_unknown_fields(&written, &known),
        })
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
        let dir = crate::test_temp_dir().join(format!("kame-db-test-{}", Uuid::new_v4()));
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

    /// Notebooks arrive from other machines, so a link could be waiting at a log's name. SQLite
    /// removes one beside a new database and refuses to open an existing one with it. kame relies
    /// on that, so this says it still holds.
    #[test]
    fn a_symbolic_link_beside_a_document_cannot_take_its_writes_elsewhere() {
        let write = |path: &Path| -> Result<(), DbError> {
            let mut db = Db::open(path)?;
            db.save_document(&sample_document())
        };
        for suffix in ["-wal", "-shm", "-journal"] {
            for already_written in [false, true] {
                for target_exists in [false, true] {
                    let path = temp_db_path();
                    let outside = path.with_file_name("outside");
                    if already_written {
                        write(&path).unwrap();
                    }
                    if target_exists {
                        std::fs::write(&outside, "left alone").unwrap();
                    }
                    let link = format!("{}{suffix}", path.display());
                    std::os::unix::fs::symlink(&outside, link).unwrap();

                    let _ = write(&path);

                    let case = format!("{suffix}, written {already_written}");
                    if target_exists {
                        let held = std::fs::read_to_string(&outside).unwrap();
                        assert_eq!(held, "left alone", "{case}");
                    } else {
                        assert!(!outside.exists(), "{case}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_newer_file_is_refused_when_only_reading_it() {
        let path = temp_db_path();
        {
            let mut db = Db::open(&path).unwrap();
            db.save_document(&sample_document()).unwrap();
            db.conn.pragma_update(None, "user_version", 99_i64).unwrap();
        }
        assert!(matches!(
            Db::open_read_only(&path),
            Err(DbError::SchemaTooNew { found: 99, .. })
        ));
    }

    /// Every kind of step of history this build writes, holding every kind of block, style and
    /// note, with each optional part both set and left at its default. None of it is taken for a
    /// newer build's, or every file would open read-only.
    #[test]
    fn no_step_of_history_this_build_writes_is_taken_for_a_newer_one() {
        use crate::model::{
            Anchor, Annotation, AnnotationKind, Block, BlockKind, ImageAlign, Span, SpanStyle,
        };
        let kinds = vec![
            BlockKind::Paragraph,
            BlockKind::Heading {
                level: 2,
                centered: false,
            },
            BlockKind::Heading {
                level: 3,
                centered: true,
            },
            BlockKind::ListItem {
                ordered: false,
                depth: 0,
            },
            BlockKind::ListItem {
                ordered: true,
                depth: 2,
            },
            BlockKind::BlockQuote,
            BlockKind::Image {
                path: "a.png".to_string(),
                link: None,
                align: ImageAlign::Left,
            },
            BlockKind::Image {
                path: "b.png".to_string(),
                link: Some("https://b.example/".to_string()),
                align: ImageAlign::Right,
            },
            BlockKind::Rule,
            BlockKind::Code { language: None },
            BlockKind::Code {
                language: Some("rust".to_string()),
            },
            BlockKind::Prose,
        ];
        let spans = vec![
            Span {
                start: 0,
                end: 1,
                style: SpanStyle::Bold,
            },
            Span {
                start: 1,
                end: 2,
                style: SpanStyle::Italic,
            },
            Span {
                start: 2,
                end: 3,
                style: SpanStyle::Code,
            },
            Span {
                start: 3,
                end: 4,
                style: SpanStyle::Link {
                    url: "https://a.example/".to_string(),
                },
            },
            Span {
                start: 4,
                end: 5,
                style: SpanStyle::DocumentLink {
                    target: "other.kame#Heading".to_string(),
                },
            },
        ];
        let blocks: Vec<Block> = kinds
            .into_iter()
            .map(|kind| Block {
                id: Uuid::new_v4(),
                kind,
                text: "words".to_string(),
                spans: spans.clone(),
            })
            .collect();
        let block_id = blocks[0].id;
        let notes: Vec<Annotation> = [
            (AnnotationKind::Marginal, Anchor::block(block_id)),
            (AnnotationKind::Note, Anchor::range(block_id, 0, 4)),
            (AnnotationKind::Footnote, Anchor::range(block_id, 4, 4)),
        ]
        .into_iter()
        .map(|(kind, anchor)| Annotation {
            id: Uuid::new_v4(),
            kind,
            anchor,
            body: "a note".to_string(),
        })
        .collect();
        let mut document = Document::new("t");
        document.blocks = blocks.clone();
        document.annotations = notes.clone();
        let mut ops = Vec::new();
        for (index, block) in blocks.iter().enumerate() {
            ops.push(Op::InsertBlock {
                index,
                block: block.clone(),
            });
            ops.push(Op::DeleteBlock {
                index,
                block: block.clone(),
                annotations: notes.clone(),
            });
            ops.push(Op::SetBlockKind {
                block_id: block.id,
                old_kind: BlockKind::Paragraph,
                kind: block.kind.clone(),
            });
        }
        for note in &notes {
            ops.push(Op::AddAnnotation {
                annotation: note.clone(),
            });
            ops.push(Op::RemoveAnnotation {
                annotation: note.clone(),
            });
        }
        ops.extend([
            Op::InsertText {
                block_id,
                offset: 0,
                text: "é".to_string(),
            },
            Op::DeleteText {
                block_id,
                start: 0,
                end: 2,
                text: "é".to_string(),
                removed: Vec::new(),
                old_spans: None,
                clipped: Vec::new(),
            },
            Op::DeleteText {
                block_id,
                start: 0,
                end: 2,
                text: "é".to_string(),
                removed: notes.clone(),
                old_spans: Some(spans.clone()),
                clipped: notes.clone(),
            },
            Op::SetSpans {
                block_id,
                old_spans: Vec::new(),
                spans: spans.clone(),
            },
            Op::SetAnnotationBody {
                annotation_id: notes[0].id,
                old_body: "a".to_string(),
                new_body: "b".to_string(),
            },
            Op::SetTitle {
                old: "a".to_string(),
                new: "b".to_string(),
            },
            Op::ReplaceDocument {
                old: Box::new(Document::new("old")),
                new: Box::new(document.clone()),
            },
        ]);
        let batch = Op::Batch { ops: ops.clone() };
        ops.push(batch);

        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();
        db.save_document(&document).unwrap();
        for op in &ops {
            let json = serde_json::to_string(op).unwrap();
            assert!(!drops_something::<Op>(&json), "{json}");
            db.append_op(op).unwrap();
        }
        assert!(!db.holds_newer());
    }

    #[test]
    fn forgetting_an_edit_moves_log_head_and_cursor_together() {
        let path = temp_db_path();
        let first = sample_document();
        let mut second = first.clone();
        second.title = "Second".to_string();
        let mut db = Db::open(&path).unwrap();
        db.record_edit(&first, &insert_op(0)).unwrap();
        db.record_edit(&second, &insert_op(1)).unwrap();

        db.forget_last_edit(&first, 1).unwrap();
        drop(db);

        let db = Db::open(&path).unwrap();
        assert_eq!(db.load_document().unwrap(), first);
        assert_eq!(db.load_ops().unwrap().len(), 1);
        assert_eq!(db.cursor().unwrap(), 1);
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

    /// A block an op brings back is made to fit as the head is, and a delete's old spans, given
    /// against text the op doesn't carry, are kept as they were.
    #[test]
    fn blocks_in_the_log_are_read_fitted() {
        use crate::model::{Block, BlockKind, Span, SpanStyle};
        let mut db = Db::open_in_memory().unwrap();
        let bold = |start, end| Span {
            start,
            end,
            style: SpanStyle::Bold,
        };
        let block = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "Café".to_string(),
            spans: vec![bold(0, 4), bold(9, 2)],
        };
        let delete = Op::DeleteText {
            block_id: block.id,
            start: 0,
            end: 5,
            text: "Café".to_string(),
            removed: Vec::new(),
            old_spans: Some(vec![bold(0, 40)]),
            clipped: Vec::new(),
        };
        db.append_op(&Op::Batch {
            ops: vec![
                Op::InsertBlock {
                    index: 1,
                    block: block.clone(),
                },
                delete.clone(),
            ],
        })
        .unwrap();

        let log = db.load_ops().unwrap();

        let fitted = Block {
            spans: vec![bold(0, 3)],
            ..block
        };
        let read = Op::Batch {
            ops: vec![
                Op::InsertBlock {
                    index: 1,
                    block: fitted,
                },
                delete,
            ],
        };
        assert_eq!(
            log.into_iter().map(|(_, op)| op).collect::<Vec<_>>(),
            [read]
        );
    }

    /// Fitting is for what doesn't fit, so a log kame wrote reads back exactly as it was.
    #[test]
    fn a_log_that_fits_reads_back_unchanged() {
        use crate::model::{Anchor, Annotation, AnnotationKind, Block, BlockKind, Document};
        use crate::model::{Span, SpanStyle};
        let span = |start, end| Span {
            start,
            end,
            style: SpanStyle::Italic,
        };
        let block = Block {
            id: Uuid::new_v4(),
            kind: BlockKind::Paragraph,
            text: "hi 🙂 café".to_string(),
            spans: vec![span(3, 7), span(7, 7), span(8, 13)],
        };
        let note = |start, end| Annotation {
            id: Uuid::new_v4(),
            kind: AnnotationKind::Note,
            anchor: Anchor::range(block.id, start, end),
            body: "a note".to_string(),
        };
        let mut doc = Document::new("Fits");
        doc.blocks.push(block.clone());
        doc.annotations = vec![note(3, 7), note(13, 13)];
        let log = vec![
            Op::InsertBlock {
                index: 1,
                block: block.clone(),
            },
            Op::Batch {
                ops: vec![
                    Op::DeleteText {
                        block_id: block.id,
                        start: 3,
                        end: 13,
                        text: "🙂 café".to_string(),
                        removed: vec![note(3, 7), note(13, 13)],
                        old_spans: Some(block.spans.clone()),
                        clipped: vec![note(0, 7)],
                    },
                    Op::DeleteBlock {
                        index: 1,
                        block: block.clone(),
                        annotations: doc.annotations.clone(),
                    },
                ],
            },
            Op::ReplaceDocument {
                old: Box::new(doc.clone()),
                new: Box::new(doc),
            },
        ];
        let mut db = Db::open_in_memory().unwrap();
        for op in &log {
            db.append_op(op).unwrap();
        }
        let read: Vec<Op> = db
            .load_ops()
            .unwrap()
            .into_iter()
            .map(|(_, op)| op)
            .collect();
        assert_eq!(read, log);
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

    /// A file closed to be renamed holds everything written to it by itself, without its `-wal`,
    /// and closing one fails while that can't be so, as when something is still reading the log.
    #[test]
    fn closing_puts_the_whole_log_into_the_file_or_fails() {
        let path = temp_db_path();
        let mut changed = sample_document();
        changed.title = "Changed".to_string();
        // Held open so that closing the other connection doesn't take the log away with it.
        let other = Connection::open(&path).unwrap();
        let mut db = Db::open(&path).unwrap();
        db.save_document(&changed).unwrap();
        db.close().unwrap();
        let alone = path.with_file_name("alone.kame");
        std::fs::copy(&path, &alone).unwrap();
        assert_eq!(
            Db::open_read_only(&alone).unwrap().load_document().unwrap(),
            changed
        );

        let mut db = Db::open(&path).unwrap();
        other.execute_batch("BEGIN").unwrap();
        let _: i64 = other
            .query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
            .unwrap();
        db.save_document(&sample_document()).unwrap();
        db.conn
            .busy_timeout(std::time::Duration::from_millis(10))
            .unwrap();
        assert!(db.close().is_err());
        other.execute_batch("COMMIT").unwrap();
    }

    /// A history said again changes each snapshot named in place, keeping its label and time, and
    /// one that isn't there leaves the head, the log, the cursor and every snapshot as they were.
    #[test]
    fn replacing_the_history_says_its_snapshots_again_or_nothing() {
        let path = temp_db_path();
        let mut db = Db::open(&path).unwrap();
        let doc = sample_document();
        db.save_document(&doc).unwrap();
        db.append_op(&insert_op(0)).unwrap();
        let id = db.create_snapshot(&doc, Some("kept")).unwrap();
        let listed = db.list_snapshots().unwrap();
        let mut again = doc.clone();
        again.title = "Said again".to_string();

        assert!(matches!(
            db.replace_history(
                &again,
                &[],
                0,
                &Default::default(),
                &[(id, again.clone()), (id + 1, again.clone())]
            ),
            Err(DbError::SnapshotNotFound(missing)) if missing == id + 1
        ));
        assert_eq!(db.load_document().unwrap(), doc);
        assert_eq!(db.load_ops().unwrap().len(), 1);
        assert_eq!(db.restore_snapshot(id).unwrap(), doc);

        db.replace_history(&again, &[], 0, &Default::default(), &[(id, again.clone())])
            .unwrap();
        assert_eq!(db.load_document().unwrap(), again);
        assert!(db.load_ops().unwrap().is_empty());
        assert_eq!(db.restore_snapshot(id).unwrap(), again);
        assert_eq!(db.list_snapshots().unwrap(), listed);
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
            ..crate::model::Metadata::default()
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
        let dir = crate::test_temp_dir().join(format!("kame-meta-{}", uuid::Uuid::new_v4()));
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

    #[test]
    fn a_sync_place_round_trips_and_is_cleared_with_the_state() {
        let mut db = Db::open_in_memory().unwrap();
        assert_eq!(db.load_sync_place(), None);
        db.save_sync_state("r").unwrap();
        db.save_sync_place("essays/a.kame").unwrap();
        assert_eq!(db.load_sync_state().as_deref(), Some("r"));
        assert_eq!(db.load_sync_place().as_deref(), Some("essays/a.kame"));
        db.save_sync_place("b.kame").unwrap();
        assert_eq!(db.load_sync_place().as_deref(), Some("b.kame"));
        db.clear_sync_state().unwrap();
        assert_eq!(db.load_sync_state(), None);
        assert_eq!(db.load_sync_place(), None);
    }

    #[test]
    fn a_sync_state_and_place_are_saved_together_or_not_at_all() {
        let mut db = Db::open_in_memory().unwrap();
        db.save_sync("r1", "first.kame").unwrap();
        assert_eq!(db.load_sync_state().as_deref(), Some("r1"));
        assert_eq!(db.load_sync_place().as_deref(), Some("first.kame"));

        for key in [SYNC_KEY, SYNC_PLACE_KEY] {
            db.conn
                .execute_batch(&format!(
                    "CREATE TRIGGER refuse BEFORE INSERT ON meta WHEN NEW.key = '{key}' \
                     BEGIN SELECT RAISE(ABORT, 'refused'); END;"
                ))
                .unwrap();
            assert!(db.save_sync("r2", "second.kame").is_err(), "{key}");
            assert_eq!(db.load_sync_state().as_deref(), Some("r1"), "{key}");
            assert_eq!(db.load_sync_place().as_deref(), Some("first.kame"), "{key}");
            db.conn.execute_batch("DROP TRIGGER refuse").unwrap();
        }
    }

    #[test]
    fn a_position_round_trips_and_starts_absent() {
        let mut db = Db::open_in_memory().unwrap();
        assert_eq!(db.load_position(), None);
        let position = Position {
            block_id: uuid::Uuid::new_v4(),
            offset: 7,
            scroll: 120.5,
        };
        db.save_position(&position).unwrap();
        assert_eq!(db.load_position(), Some(position));
    }

    /// Saving where the author is reading must not count as editing.
    #[test]
    fn saving_a_position_leaves_the_undo_cursor_alone() {
        let mut db = Db::open_in_memory().unwrap();
        let before = db.cursor().unwrap();
        db.save_position(&Position {
            block_id: uuid::Uuid::new_v4(),
            offset: 0,
            scroll: 0.0,
        })
        .unwrap();
        assert_eq!(db.cursor().unwrap(), before);
    }

    fn saved_file(name: &str) -> std::path::PathBuf {
        let path =
            crate::test_temp_dir().join(format!("kame-{name}-{}.kame", uuid::Uuid::new_v4()));
        let mut db = Db::open(&path).unwrap();
        db.save_document(&Document::new("Saved")).unwrap();
        drop(db);
        path
    }

    #[test]
    fn a_read_only_open_reads_the_document() {
        let path = saved_file("read");
        let db = Db::open_read_only(&path).unwrap();
        assert_eq!(db.load_document().unwrap().title, "Saved");
    }

    #[test]
    fn a_read_only_open_leaves_the_file_as_it_was() {
        let path = saved_file("untouched");
        let before = std::fs::read(&path).unwrap();
        let db = Db::open_read_only(&path).unwrap();
        db.load_document().unwrap();
        let _ = db.load_metadata();
        drop(db);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn a_read_only_open_refuses_to_write() {
        let path = saved_file("refuses");
        let mut db = Db::open_read_only(&path).unwrap();
        assert!(db.save_document(&Document::new("Changed")).is_err());
    }

    #[test]
    fn a_read_only_open_doesnt_create_a_missing_file() {
        let path = crate::test_temp_dir().join(format!("kame-gone-{}.kame", uuid::Uuid::new_v4()));
        assert!(Db::open_read_only(&path).is_err());
        assert!(!path.exists());
    }

    fn sidecars(path: &Path) -> Vec<String> {
        ["-wal", "-shm"]
            .into_iter()
            .map(|suffix| format!("{}{suffix}", path.display()))
            .filter(|sidecar| Path::new(sidecar).exists())
            .collect()
    }

    /// Reading must leave nothing behind: documents live in synced folders, and files
    /// appearing beside them would spread.
    #[test]
    fn a_read_only_open_leaves_nothing_beside_the_file() {
        let path = saved_file("sidecars");
        assert!(sidecars(&path).is_empty(), "the fixture closed cleanly");
        let db = Db::open_read_only(&path).unwrap();
        db.load_document().unwrap();
        let _ = db.load_metadata();
        drop(db);
        assert!(sidecars(&path).is_empty(), "{:?}", sidecars(&path));
    }

    #[test]
    fn a_read_only_open_sees_what_a_writer_hasnt_checkpointed() {
        let path = saved_file("writer");
        let mut writer = Db::open(&path).unwrap();
        writer.save_document(&Document::new("Newer")).unwrap();
        let reader = Db::open_read_only(&path).unwrap();
        assert_eq!(reader.load_document().unwrap().title, "Newer");
    }

    #[test]
    fn a_read_only_open_copes_with_an_awkward_file_name() {
        let dir = crate::test_temp_dir().join(format!("kame-awkward-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("été 100% ok?#1.kame");
        let mut db = Db::open(&path).unwrap();
        db.save_document(&Document::new("Awkward")).unwrap();
        drop(db);
        let reader = Db::open_read_only(&path).unwrap();
        assert_eq!(reader.load_document().unwrap().title, "Awkward");
        drop(reader);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A `-wal` with no `-shm`, as a crash or a sync can leave: the text in it has to be read,
    /// and reading it in place would create a `-shm` beside the document.
    fn crashed_copy(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let path = saved_file(name);
        let mut writer = Db::open(&path).unwrap();
        writer.save_document(&Document::new("In the wal")).unwrap();
        let dir = crate::test_temp_dir().join(format!("kame-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("a.kame");
        std::fs::copy(&path, &copy).unwrap();
        std::fs::copy(
            format!("{}-wal", path.display()),
            format!("{}-wal", copy.display()),
        )
        .unwrap();
        drop(writer);
        (dir, copy)
    }

    #[test]
    fn a_read_only_open_after_a_crash_reads_the_wal_and_adds_nothing() {
        let (dir, copy) = crashed_copy("crashed");
        let wal = std::fs::read(format!("{}-wal", copy.display())).unwrap();
        let before = std::fs::read(&copy).unwrap();
        let db = Db::open_read_only(&copy).unwrap();
        assert_eq!(db.load_document().unwrap().title, "In the wal");
        drop(db);
        assert!(!Path::new(&format!("{}-shm", copy.display())).exists());
        assert_eq!(std::fs::read(&copy).unwrap(), before);
        assert_eq!(
            std::fs::read(format!("{}-wal", copy.display())).unwrap(),
            wal
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_read_only_open_after_a_crash_works_in_a_read_only_directory() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, copy) = crashed_copy("crashedro");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let read = Db::open_read_only(&copy).and_then(|db| db.load_document());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(read.unwrap().title, "In the wal");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_read_only_open_cleans_up_after_itself() {
        let (dir, copy) = crashed_copy("crashedclean");
        let db = Db::open_read_only(&copy).unwrap();
        let scratch = db.scratch.as_ref().map(|scratch| scratch.0.clone());
        assert!(scratch.as_ref().is_some_and(|scratch| scratch.exists()));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(scratch.as_ref().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "a copy of someone's writing stays private"
        );
        drop(db);
        assert!(!scratch.unwrap().exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replacing_the_last_edit_changes_it_and_the_head_together() {
        let mut db = Db::open_in_memory().unwrap();
        let id = uuid::Uuid::new_v4();
        let first = Op::InsertText {
            block_id: id,
            offset: 0,
            text: "a".to_string(),
        };
        let second = Op::InsertText {
            block_id: id,
            offset: 1,
            text: "b".to_string(),
        };
        let joined = Op::InsertText {
            block_id: id,
            offset: 1,
            text: "bc".to_string(),
        };
        db.record_edit(&Document::new("One"), &first).unwrap();
        db.record_edit(&Document::new("Two"), &second).unwrap();
        db.replace_last_edit(&Document::new("Three"), &joined)
            .unwrap();
        let ops: Vec<Op> = db
            .load_ops()
            .unwrap()
            .into_iter()
            .map(|(_, op)| op)
            .collect();
        assert_eq!(ops, vec![first, joined]);
        assert_eq!(db.cursor().unwrap(), 2);
        assert_eq!(db.load_document().unwrap().title, "Three");
    }

    #[test]
    fn replacing_the_log_replaces_it_and_the_cursor_together() {
        let mut db = Db::open_in_memory().unwrap();
        let id = uuid::Uuid::new_v4();
        let op = |text: &str| Op::InsertText {
            block_id: id,
            offset: 0,
            text: text.to_string(),
        };
        db.record_edit(&Document::new("A"), &op("a")).unwrap();
        db.record_edit(&Document::new("B"), &op("b")).unwrap();
        db.replace_log(&[op("joined"), op("redo")], 1).unwrap();
        let ops: Vec<Op> = db
            .load_ops()
            .unwrap()
            .into_iter()
            .map(|(_, op)| op)
            .collect();
        assert_eq!(ops, vec![op("joined"), op("redo")]);
        assert_eq!(db.cursor().unwrap(), 1);
    }
}
