//! An open document: its editor, its file, the caret and selection, and how every edit
//! reaches the disk. No window is involved, so anything can drive one.

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use crate::edit::{Cursor, EditError, Editor, Op, Selection};
use crate::import::markdown::{from_markdown, Imported};
use crate::links::{
    image_file, names_something, own_target_after_save_as, retargeted, stored_image_path,
};
use crate::model::{
    bare_link_ranges, Anchor, AnnotationId, AnnotationKind, BlockId, BlockKind, Document,
    ImageAlign, Metadata, Span, SpanStyle,
};
use crate::references::{
    block_citation_ranges, citation_ranges, key_of, rewrite_citation, valid_key, References,
};
use crate::storage::{self, Db, DbError, SnapshotMeta};
use crate::text::{next_boundary, next_word, prev_boundary, prev_word, widen_to_words, word_at};

/// The document, its durable history, and the caret — everything that exists
/// without a window.
///
/// Every mutation goes through one of the command methods, which apply the
/// edit, persist it, and move the caret. That keeps the GUI layer free of
/// persistence rules and lets the whole editing loop be tested headlessly.
pub struct Session {
    pub editor: Editor,
    pub db: Db,
    pub caret: Cursor,
    /// A dragged/shift-clicked range, oriented from where the drag began.
    /// `caret` is always the moving end; `selection == None` means a bare
    /// caret.
    pub selection: Option<Selection>,
    pub last_error: Option<String>,
    /// Set when a write failed. Further mutations are refused until a write
    /// succeeds, so the in-memory undo cursor cannot drift from the DB cursor.
    pub save_failed: bool,
    /// The history as it stood after another machine's words last arrived, for telling whether
    /// anything has happened since.
    followed_at: Option<(usize, usize)>,
    /// The file this document was imported from.
    pub source_path: Option<PathBuf>,
    /// Where the page was scrolled when this document was last left.
    pub restored_scroll: Option<f32>,
    /// A document just begun, not yet given the default author.
    pub is_new: bool,
    /// What the document says about itself. Saved directly, not an undo step.
    pub metadata: Metadata,
    /// The file this session is persisted to, or `None` for an anonymous
    /// (in-memory) document that has not been saved yet. Exports land beside
    /// it, and the snapshots it prompts for live in the database it names.
    pub path: Option<PathBuf>,
    /// Which file `path` named when it was opened.
    on_disk: Option<FileIdentity>,
    /// Which file its `-wal` was, as of the last write, so as to tell it from one that arrived.
    log_on_disk: Option<FileIdentity>,
    /// Set when `path` stopped being that file. Nothing more is written.
    pub replaced: bool,
    /// Whether this document holds something from a newer version of kame. Saving would lose it,
    /// so nothing is saved until kame is updated.
    pub newer: bool,
}

/// Where SQLite keeps the log of writes to the database at `path`.
fn wal_path(path: &Path) -> PathBuf {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    PathBuf::from(wal)
}

/// Which file a path leads to, as opposed to what is in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

fn file_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let found = std::fs::metadata(path).ok()?;
    Some(FileIdentity {
        device: found.dev(),
        inode: found.ino(),
    })
}

impl Session {
    /// Opens the database at `path`, creating a fresh document when it holds
    /// none yet, and resumes the editor at the persisted undo cursor.
    ///
    /// An unreadable op log is a degraded open, not a failure: the head
    /// document is the user's text, and history is only how it got there. One
    /// row written by a future build (or a partially written one) must not make
    /// the document unopenable, so the session starts with no history and a
    /// non-fatal warning, and the durable state is reset to match that empty
    /// history.
    pub fn open(path: &Path) -> AppResult<Session> {
        let mut db = Db::open(path).map_err(|err| open_error(path, err))?;
        let mut warning = None;
        // Set when the degrade path could not reset the database, which leaves
        // it unable to write rather than merely historyless.
        let mut reset_error: Option<DbError> = None;
        let mut fresh = false;
        let newer = db.holds_newer();
        let editor = match db.load_document() {
            Ok(document) => match db.load_ops() {
                Ok(ops) => {
                    let ops: Vec<Op> = ops.into_iter().map(|(_, op)| op).collect();
                    let cursor = db.cursor()?;
                    // Resume with the persisted cursor, never with_history: the
                    // saved head may already have ops undone, and treating the
                    // whole log as applied would offer those ops for undo a
                    // second time.
                    Editor::with_cursor(document, ops, cursor)
                }
                // It belongs to a newer version of kame, and that version can still use it.
                Err(_) if newer => Editor::new(document),
                Err(err) => {
                    warning = Some(format!(
                        "history unavailable — {err}; opening the document with no undo history"
                    ));
                    let editor = Editor::new(document);
                    // An in-memory editor with no history beside a DB cursor
                    // still pointing into the unreadable log would desync on the
                    // first edit: the edit truncates the log as a redo tail
                    // (shifting the sequence numbers the cursor counts), and the
                    // next undo writes the in-memory count as the DB cursor, so
                    // a reopen puts a surviving op on the redo stack and redo
                    // re-applies it to text it never touched. The unreadable ops
                    // are unrecoverable anyway — the first edit would drop them —
                    // so reset the DB to the degraded editor in one transaction:
                    // head, cursor 0, empty log.
                    if let Err(err) = db.restore_state(editor.document()) {
                        reset_error = Some(err);
                    }
                    editor
                }
            },
            Err(DbError::NoDocument) => {
                fresh = true;
                let title = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("Untitled");
                let document = Document::new(title);
                db.save_document(&document)?;
                Editor::new(document)
            }
            Err(err) => return Err(Box::new(open_error(path, err))),
        };

        let caret = editor
            .document()
            .blocks
            .first()
            .map(|block| Cursor {
                block_id: block.id,
                offset: 0,
            })
            .ok_or("document has no blocks")?;
        // Pick up where this document was left, unless that block has since gone.
        let position = db.load_position().filter(|position| {
            editor
                .document()
                .blocks
                .iter()
                .any(|block| block.id == position.block_id)
        });
        let caret = position.map_or(caret, |position| {
            clamp_cursor(
                editor.document(),
                Cursor {
                    block_id: position.block_id,
                    offset: position.offset,
                },
            )
        });
        // A reset that failed leaves the unreadable log on disk beside an
        // editor with no history. Do not open a session whose history could
        // diverge from the DB: open it read-only, so the first edit cannot
        // shift the log under the cursor.
        let (last_error, save_failed) = match reset_error {
            Some(err) => (Some(format!("save failed — editing disabled: {err}")), true),
            None => (warning, false),
        };
        let metadata = match db.load_metadata() {
            Ok(Some(metadata)) => metadata,
            // Missing or unreadable: the text is what matters, so fall back, and
            // keep the defaults for next time if the file will take them.
            _ => {
                let metadata = default_metadata(&editor.document().title, path);
                let _ = db.save_metadata(&metadata);
                metadata
            }
        };
        let mut session = Session {
            is_new: fresh,
            editor,
            db,
            caret,
            selection: None,
            last_error,
            save_failed,
            followed_at: None,
            source_path: None,
            restored_scroll: position.map(|position| position.scroll),
            metadata,
            path: Some(path.to_path_buf()),
            on_disk: file_identity(path),
            log_on_disk: file_identity(&wal_path(path)),
            replaced: false,
            newer: false,
        };
        if newer {
            session.mark_newer();
        }
        Ok(session)
    }

    /// Stops saving this document, because it holds something from a newer version of kame that
    /// saving would lose. It stays on the screen to read.
    pub fn mark_newer(&mut self) {
        self.newer = true;
        self.save_failed = true;
        self.last_error = Some(
            "this document has changes from a newer version of kame, so it's read-only here \
             until kame is updated"
                .to_string(),
        );
    }

    /// A session for a document that could not be opened: a fresh, in-memory
    /// document so the author still has somewhere to type, with `error` shown
    /// in the status panel. The failed file is never touched, so a locked or
    /// corrupt document cannot be overwritten by a stray save.
    pub fn blank(path: &Path, error: String) -> AppResult<Session> {
        let db = Db::open_in_memory()?;
        let title = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Untitled");
        let editor = Editor::new(Document::new(title));
        let caret = editor
            .document()
            .blocks
            .first()
            .map(|block| Cursor {
                block_id: block.id,
                offset: 0,
            })
            .ok_or("document has no blocks")?;
        Ok(Session {
            is_new: false,
            editor,
            db,
            caret,
            selection: None,
            last_error: Some(error),
            save_failed: false,
            followed_at: None,
            source_path: None,
            restored_scroll: None,
            metadata: Metadata {
                created: today(),
                ..Metadata::default()
            },
            path: None,
            on_disk: None,
            log_on_disk: None,
            replaced: false,
            newer: false,
        })
    }

    /// A fresh anonymous document for a launch with no file argument: an
    /// in-memory database with no path, so the author starts typing at once and
    /// the first save prompts for a file (see [`Session::save_as`]).
    pub fn anonymous() -> AppResult<Session> {
        let db = Db::open_in_memory()?;
        let editor = Editor::new(Document::new("Untitled"));
        let caret = editor
            .document()
            .blocks
            .first()
            .map(|block| Cursor {
                block_id: block.id,
                offset: 0,
            })
            .ok_or("document has no blocks")?;
        Ok(Session {
            is_new: true,
            editor,
            db,
            caret,
            selection: None,
            last_error: None,
            save_failed: false,
            followed_at: None,
            source_path: None,
            restored_scroll: None,
            metadata: Metadata {
                created: today(),
                ..Metadata::default()
            },
            path: None,
            on_disk: None,
            log_on_disk: None,
            replaced: false,
            newer: false,
        })
    }

    /// Opens whatever `path` names: Markdown is imported, anything else is a
    /// `.kame` document. An import also returns a line for the author saying
    /// what happened to it.
    pub fn open_any(path: &Path) -> AppResult<(Session, Option<String>)> {
        if is_markdown_path(path) {
            let (session, note) = Session::import(path)?;
            Ok((session, Some(note)))
        } else {
            Ok((Session::open(path)?, None))
        }
    }

    /// A new anonymous document holding the Markdown file at `path`, which is
    /// only read. The first save asks where the `.kame` file goes.
    pub fn import(path: &Path) -> AppResult<(Session, String)> {
        let source = std::fs::read_to_string(path)?;
        // Made absolute because an anonymous document has no directory of its
        // own for a relative image path to resolve against.
        let path = std::path::absolute(path)?;
        let base_dir = path.parent().unwrap_or(Path::new(""));
        let title = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Untitled");
        let Imported {
            document,
            mut metadata,
            report,
        } = from_markdown(&source, base_dir, title);
        if metadata.title.is_empty() {
            metadata.title = document.title.clone();
        }
        metadata.created = today();

        let mut note = format!(
            "imported {} — not saved yet",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        if let Some(summary) = report.summary() {
            note.push_str(&format!("; {summary}"));
        }
        let missing = missing_images(&document);
        if missing > 0 {
            let noun = if missing == 1 { "image" } else { "images" };
            note.push_str(&format!("; {missing} {noun} not found"));
        }

        let mut db = Db::open_in_memory()?;
        db.save_metadata(&metadata)?;
        let editor = Editor::new(document);
        let caret = editor
            .document()
            .blocks
            .first()
            .map(|block| Cursor {
                block_id: block.id,
                offset: 0,
            })
            .ok_or("document has no blocks")?;
        let session = Session {
            is_new: true,
            editor,
            db,
            caret,
            selection: None,
            last_error: None,
            save_failed: false,
            followed_at: None,
            source_path: Some(path.clone()),
            restored_scroll: None,
            metadata,
            path: None,
            on_disk: None,
            log_on_disk: None,
            replaced: false,
            newer: false,
        };
        Ok((session, note))
    }

    /// Whether replacing this document would throw away work: edits a failed
    /// save left only in memory, or an anonymous document with no file at all.
    pub fn would_lose_work(&self) -> bool {
        (self.save_failed && !self.newer) || (self.is_anonymous() && self.has_content())
    }

    /// The name to show for this document: its metadata's title, or its file's
    /// name if it has none.
    pub fn display_title(&self) -> String {
        if !self.metadata.title.is_empty() {
            return self.metadata.title.clone();
        }
        self.path()
            .and_then(Path::file_stem)
            .and_then(|stem| stem.to_str())
            .unwrap_or("Untitled")
            .to_string()
    }

    /// Replaces the metadata and saves it at once. It is not an edit: it takes no
    /// part in undo, and a failure to write costs nothing but the report.
    /// Gives a document just begun the default author, unless it came with one. A document
    /// that already exists keeps what it has, even if that is nothing.
    pub fn apply_default_author(&mut self, author: &str) {
        if !std::mem::take(&mut self.is_new)
            || author.is_empty()
            || !self.metadata.author.is_empty()
        {
            return;
        }
        let mut metadata = self.metadata.clone();
        metadata.author = author.to_string();
        self.set_metadata(metadata);
    }

    pub fn set_metadata(&mut self, metadata: Metadata) {
        if self.newer {
            return;
        }
        self.metadata = metadata;
        if let Err(err) = self.db.save_metadata(&self.metadata) {
            self.last_error = Some(format!("could not save document properties — {err}"));
        }
    }

    /// The metadata as an export publishes it, with the title the document goes by.
    pub fn published_metadata(&self) -> Metadata {
        let title = if self.metadata.title.is_empty() && self.is_anonymous() {
            String::new()
        } else {
            self.display_title()
        };
        Metadata {
            title,
            ..self.metadata.clone()
        }
    }

    /// Where an export dialog starts: beside the document, or an unsaved import's
    /// source, under the same name.
    pub fn export_suggestion(&self, extension: &str) -> PathBuf {
        export_path(&self.export_base(), extension)
    }

    /// The directory a file dialog starts in: the document's, or an unsaved
    /// import's source's. A new document leaves it to the system.
    pub fn dialog_directory(&self) -> Option<PathBuf> {
        self.path
            .as_deref()
            .or(self.source_path.as_deref())
            .and_then(Path::parent)
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(Path::to_path_buf)
    }

    /// Records where the author is, for a document with a file to keep it in. Losing
    /// this costs nothing but the place, so a failed write is not reported.
    pub fn save_position(&mut self, scroll: f32) {
        if self.path.is_some() {
            let _ = self.db.save_position(&storage::Position {
                block_id: self.caret.block_id,
                offset: self.caret.offset,
                scroll,
            });
        }
    }

    /// What a sync tool last recorded for this document.
    pub fn sync_state(&self) -> Option<String> {
        self.db.load_sync_state()
    }

    /// Records this for a sync tool. Losing it only costs that tool some work, so a failed write
    /// is not reported.
    pub fn set_sync_state(&mut self, state: &str) {
        let _ = self.db.save_sync_state(state);
    }

    /// Records what a sync tool got to and where, together. A failed write isn't reported, as with
    /// the state, and leaves both as they were.
    pub fn set_sync(&mut self, state: &str, place: &str) {
        let _ = self.db.save_sync(state, place);
    }

    /// Where a sync tool last synced this document, in its own terms.
    pub fn sync_place(&self) -> Option<String> {
        self.db.load_sync_place()
    }

    /// Records this for a sync tool. A failed write isn't reported, as with the state.
    pub fn set_sync_place(&mut self, place: &str) {
        let _ = self.db.save_sync_place(place);
    }

    /// What the first save suggests calling the file: the source's name for an
    /// import, whatever title its front matter gave the document.
    pub fn suggested_file_name(&self) -> String {
        let stem = self
            .path
            .as_deref()
            .or(self.source_path.as_deref())
            .and_then(Path::file_stem)
            .and_then(|stem| stem.to_str())
            .unwrap_or("Untitled");
        format!("{stem}.kame")
    }

    pub fn document(&self) -> &Document {
        self.editor.document()
    }

    /// The `.kame` file this session is persisted to, or `None` for an
    /// anonymous (unsaved) document.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether this document has no file yet (launched with no argument, or a
    /// failed open) — the signal that saving must ask for a path first.
    pub fn is_anonymous(&self) -> bool {
        self.path.is_none()
    }

    /// Whether an anonymous document has anything worth saving: any prose or
    /// apparatus. An empty Untitled document can close without a prompt.
    pub fn has_content(&self) -> bool {
        let doc = self.editor.document();
        doc.blocks.iter().any(|block| {
            !block.text.trim().is_empty() || matches!(block.kind, BlockKind::Image { .. })
        }) || !doc.annotations.is_empty()
    }

    pub fn history(&self) -> (usize, usize) {
        self.editor.history()
    }

    // --- files, exports and snapshots --------------------------------------

    /// The path exports should land beside: the saved file's directory, or the
    /// current directory (as `document.*`) for an anonymous document.
    pub fn export_base(&self) -> PathBuf {
        self.path
            .clone()
            .or_else(|| self.source_path.clone())
            .unwrap_or_else(|| PathBuf::from("Untitled.kame"))
    }

    /// Writes the document as Markdown beside the `.kame` file and returns the
    /// path written.
    ///
    /// A failed export is reported through `last_error` but never sets
    /// `save_failed`: the document itself is still stored, so the author can
    /// keep writing and take the export again.
    pub fn export_markdown(&mut self) -> Option<PathBuf> {
        let path = self.export_suggestion("md");
        self.export_markdown_to(&path, &References::new())
    }

    /// Writes the document as Markdown to `path` and returns the path written.
    pub fn export_markdown_to(&mut self, path: &Path, references: &References) -> Option<PathBuf> {
        let markdown = crate::export::to_markdown_with(
            &self.exported_document(),
            &self.published_metadata(),
            references,
        );
        self.write_export(path, &markdown)
    }

    pub fn export_html(&mut self) -> Option<PathBuf> {
        let path = self.export_suggestion("html");
        self.export_html_to(&path, &References::new())
    }

    /// Writes the document as a standalone HTML page to `path` and returns the
    /// path written.
    pub fn export_html_to(&mut self, path: &Path, references: &References) -> Option<PathBuf> {
        let html = crate::export::to_html_with(
            &self.exported_document(),
            &self.published_metadata(),
            references,
        );
        self.write_export(path, &html)
    }

    /// The document as an export sees it. An export can land anywhere, where a path from this
    /// document's folder would lead nowhere, so its images go by their whole paths.
    fn exported_document(&self) -> Document {
        let mut document = self.editor.document().clone();
        for block in &mut document.blocks {
            if let BlockKind::Image { path, .. } = &mut block.kind {
                if let Some(whole) = whole_image_path(path, self.path.as_deref()) {
                    *path = whole.display().to_string();
                }
            }
        }
        document
    }

    /// Saves the document to `path`, carrying its history, snapshots and metadata,
    /// and switches the session to that file so later edits land there.
    ///
    /// Anything already at `path` is replaced whole: the file dialog has confirmed
    /// that. The copy is written beside it and renamed into place, so a failure
    /// leaves both the old file and this session as they were. The result is as
    /// private as the old file and the current document, whichever is more so.
    pub fn save_as(&mut self, path: &Path) -> AppResult<()> {
        if self.newer {
            return Err("this document has changes from a newer version of kame".into());
        }
        if self.replaced && self.path.as_deref() == Some(path) {
            return Err("that file changed on disk, so this needs another name".into());
        }
        if self.path.as_deref() == Some(path) {
            self.clear_save_error();
            return Ok(());
        }
        let doc = self.editor.document().clone();
        self.db.save_state(&doc, self.editor.cursor())?;
        self.db.save_metadata(&self.metadata)?;

        let name = path
            .file_name()
            .ok_or("the path does not name a file")?
            .to_string_lossy();
        let partial = path.with_file_name(format!(".{name}.saving-{}", uuid::Uuid::new_v4()));
        // Replacing a file must not widen who can read it, and a private document
        // stays private under a new name. The copy is private from the moment it
        // exists, so nobody can hold it open and read the content as it arrives,
        // and takes its final mode only once complete.
        // A document whose file has vanished may have been private, so that counts
        // as not knowing. An unsaved import answers for its source. Only a document
        // that never had a file has no mode.
        let source = match self.path.as_deref().or(self.source_path.as_deref()) {
            Some(current) => known_mode(current).and_then(|mode| mode.ok_or(()).map(Some)),
            None => Ok(None),
        };
        let mode = match (known_mode(path), source) {
            (Ok(None), Ok(None)) => default_mode(path.parent().unwrap_or(Path::new("."))),
            (Ok(target), Ok(source)) => saved_mode(target, source, PRIVATE_MODE),
            _ => PRIVATE_MODE,
        };
        let written = (|| -> AppResult<()> {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(PRIVATE_MODE)
                .open(&partial)?;
            self.db.save_copy(&partial)?;
            std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(mode))?;
            for suffix in ["-wal", "-shm"] {
                let mut stale = path.as_os_str().to_owned();
                stale.push(suffix);
                let _ = std::fs::remove_file(stale);
            }
            std::fs::rename(&partial, path)?;
            Ok(())
        })();
        if let Err(err) = written {
            let _ = std::fs::remove_file(&partial);
            return Err(err);
        }

        let mut db = Db::open(path)?;
        // The copy is a different file, and what a sync tool recorded was about the original.
        db.clear_sync_state()?;
        self.db = db;
        self.path = Some(path.to_path_buf());
        self.on_disk = file_identity(path);
        self.log_on_disk = file_identity(&wal_path(path));
        self.replaced = false;
        self.clear_save_error();
        Ok(())
    }

    /// The one write path for both exports.
    pub fn write_export(&mut self, path: &Path, contents: &str) -> Option<PathBuf> {
        match std::fs::write(path, contents) {
            Ok(()) => {
                self.clear_error_from("export failed");
                Some(path.to_path_buf())
            }
            Err(err) => {
                self.last_error = Some(format!("export failed — {}: {err}", path.display()));
                None
            }
        }
    }

    /// Stores the current document as a snapshot, labelled `label` when it has
    /// any text, and returns the new id.
    ///
    /// The snapshot is the inverse of [`Session::restore`]: it captures the
    /// text exactly as it stands, independent of the head document and the op
    /// log, so undo and redo cannot move it.
    pub fn snapshot(&mut self, label: Option<&str>) -> Option<i64> {
        let label = label.map(str::trim).filter(|label| !label.is_empty());
        match self.db.create_snapshot(self.editor.document(), label) {
            Ok(id) => {
                self.clear_error_from("snapshot failed");
                Some(id)
            }
            Err(err) => {
                self.last_error = Some(format!("snapshot failed — {err}"));
                None
            }
        }
    }

    /// The stored snapshots, newest first, or `None` when they could not be
    /// listed (the failure lands in `last_error`).
    pub fn snapshots(&mut self) -> Option<Vec<SnapshotMeta>> {
        match self.db.list_snapshots() {
            Ok(snapshots) => {
                self.clear_error_from("could not list snapshots");
                Some(snapshots)
            }
            Err(err) => {
                self.last_error = Some(format!("could not list snapshots — {err}"));
                None
            }
        }
    }

    /// Replaces the document with snapshot `id` and persists it.
    ///
    /// A restore is deliberately not an op: the snapshot is the undo point, so
    /// the restored document starts a fresh history and the op log that
    /// described the replaced text is dropped. Returns whether the restore
    /// landed; on failure the in-memory document is untouched.
    pub fn restore(&mut self, id: i64) -> bool {
        if self.newer {
            return false;
        }
        let document = match self.db.restore_snapshot(id) {
            Ok(document) => document,
            Err(err) => {
                self.last_error = Some(format!("restore failed — {err}"));
                return false;
            }
        };
        let Some(caret) = document.blocks.first().map(|block| Cursor {
            block_id: block.id,
            offset: 0,
        }) else {
            self.last_error = Some("restore failed — the snapshot holds no blocks".to_string());
            return false;
        };
        // One transaction writes the restored head, cursor 0 and an empty op
        // log. Splitting these would let a crash or I/O error land the restored
        // text beside the pre-restore log; on reopen `Editor::with_cursor`
        // would then put those stale ops on the redo stack and Ctrl-Shift-Z
        // would replay them against a document they never touched.
        if let Err(err) = self.db.restore_state(&document) {
            self.mark_save_failed(err);
            return false;
        }
        self.editor = Editor::new(document);
        self.caret = caret;
        self.selection = None;
        self.save_failed = false;
        self.last_error = None;
        true
    }

    /// Clears `last_error` when it came from the command named by `prefix`, so
    /// a retried command does not leave its old failure on screen. A save
    /// failure is never cleared here: it is a different class of problem.
    pub fn clear_error_from(&mut self, prefix: &str) {
        if self
            .last_error
            .as_deref()
            .is_some_and(|error| error.starts_with(prefix))
        {
            self.last_error = None;
        }
    }

    /// Persists the head document and the undo cursor together, the write
    /// path for undo/redo. Returns whether the write landed.
    pub fn save_state(&mut self) -> bool {
        self.check_file();
        if self.replaced || self.newer {
            return false;
        }
        match self
            .db
            .save_state(self.editor.document(), self.editor.cursor())
        {
            Ok(()) => {
                self.clear_save_error();
                true
            }
            Err(err) => {
                self.mark_save_failed(err);
                false
            }
        }
    }

    /// Persists one applied edit (op + head + cursor in one transaction).
    /// Returns whether the write landed.
    pub fn record(&mut self, op: &Op) -> bool {
        self.check_file();
        if self.replaced || self.newer {
            return false;
        }
        let saved = match self.editor.coalesced_edit() {
            Some(joined) => self.db.replace_last_edit(self.editor.document(), joined),
            None => self.db.record_edit(self.editor.document(), op).map(|_| ()),
        };
        self.log_on_disk = self
            .path
            .as_deref()
            .and_then(|path| file_identity(&wal_path(path)));
        match saved {
            Ok(_) => {
                self.clear_save_error();
                true
            }
            Err(err) => {
                self.mark_save_failed(err);
                false
            }
        }
    }

    /// Whether the file this document's path names has just stopped being the one it opened,
    /// as when a sync tool renames a new copy over it. Writes after that go to a file nobody
    /// can reach, so this makes the session read-only until it is saved elsewhere or reopened.
    /// The words stay in memory. True once, when first noticed.
    pub fn check_file(&mut self) -> bool {
        let Some(path) = self.path.as_deref() else {
            return false;
        };
        if self.replaced || file_identity(path) == self.on_disk {
            return false;
        }
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        // SQLite would pair this log with whatever file is at the path now, and it keeps it
        // there when this connection closes. The connection still has it open, so it goes on
        // working, for saving these words elsewhere.
        let log = wal_path(path);
        if self.log_on_disk.is_some() && file_identity(&log) == self.log_on_disk {
            let _ = std::fs::remove_file(&log);
            let mut index = path.as_os_str().to_owned();
            index.push("-shm");
            let _ = std::fs::remove_file(index);
        }
        self.replaced = true;
        self.save_failed = true;
        self.last_error = Some(format!(
            "{name} changed on disk, so nothing more is being saved to it"
        ));
        true
    }

    /// A failed write makes the session read-only: the in-memory history can
    /// no longer be trusted to match the DB, so further mutations would write
    /// an op log that disagrees with the head cursor.
    pub fn mark_save_failed(&mut self, err: impl std::fmt::Display) {
        self.save_failed = true;
        self.last_error = Some(format!("save failed — editing disabled: {err}"));
    }

    /// A later successful write re-establishes the head/log relationship.
    /// "A later successful write" here means `record`/`save_state`; commands
    /// are suppressed while the flag is set, so the recovery path is the
    /// error strip's retry (`save_state`) — and the window refuses to close
    /// until it lands.
    pub fn clear_save_error(&mut self) {
        if self.save_failed && !self.replaced && !self.newer {
            self.save_failed = false;
            self.last_error = None;
        }
    }

    /// The error strip's click: retry a failed write, or dismiss a message.
    ///
    /// Only a failed write has something to save, so only it retries. Every
    /// other message — the degraded-open warning, a failed export or snapshot —
    /// has no pending write: retrying there would write state the author never
    /// changed, and `clear_save_error` would not clear it anyway, leaving a
    /// permanently painted strip. Those messages are dismissed instead.
    pub fn retry_or_dismiss(&mut self) -> bool {
        if self.save_failed {
            self.save_state()
        } else {
            self.last_error = None;
            true
        }
    }

    /// Applies `result` from an editor command and persists it.
    pub fn commit(&mut self, result: Result<Op, EditError>) -> bool {
        match result {
            Ok(op) => self.record(&op),
            Err(err) => {
                self.last_error = Some(err.to_string());
                false
            }
        }
    }

    /// The caret, repaired against the current document (an undo can remove
    /// the block it points into).
    pub fn valid_caret(&mut self) -> Cursor {
        self.caret = clamp_cursor(self.editor.document(), self.caret);
        self.caret
    }

    /// Places a bare caret, dropping any selection.
    pub fn set_caret(&mut self, caret: Cursor) {
        self.caret = clamp_cursor(self.editor.document(), caret);
        self.selection = None;
    }

    /// Extends the selection's moving end to `caret`, keeping the edge where
    /// the selection began. With no selection yet, the current caret becomes
    /// the fixed edge, so a drag selects the span it covers. Collapsing back
    /// onto the fixed edge clears the selection.
    /// Selects the word at `at`, as a double click does. Where there is no word, the caret
    /// just goes there.
    /// Replaces the whole document, as one undo step, keeping the caret on something real.
    pub fn replace_document(&mut self, new: Document) -> bool {
        if self.save_failed {
            return false;
        }
        let op = Op::ReplaceDocument {
            old: Box::new(self.editor.document().clone()),
            new: Box::new(new),
        };
        let op = self.editor.apply_edit(op);
        self.selection = None;
        let saved = self.record(&op);
        self.valid_caret();
        saved
    }

    /// Rewrites every link in the document, web links, document links and an image's link, by
    /// `rewrite`, which answers `None` to leave one as it is. All of it is one undo step, and
    /// nothing is written if nothing changes. Returns how many links changed.
    pub fn rewrite_links(
        &mut self,
        mut rewrite: impl FnMut(&str) -> Option<String>,
    ) -> Result<usize, String> {
        self.rewrite_links_by_kind(|link, _| rewrite(link))
    }

    /// [`Session::rewrite_links`], telling `rewrite` whether each is a link to a document, as a
    /// `DocumentLink` is, rather than a link to anything at all.
    fn rewrite_links_by_kind(
        &mut self,
        mut rewrite: impl FnMut(&str, bool) -> Option<String>,
    ) -> Result<usize, String> {
        if self.newer {
            return Err("this document has changes from a newer version of kame".into());
        }
        if self.save_failed {
            return Err("this document could not be saved".into());
        }
        let mut document = self.editor.document().clone();
        let mut changed = 0;
        let mut apply = |link: &mut String, to_document: bool| match rewrite(link, to_document) {
            Some(new) if new != *link => {
                *link = new;
                changed += 1;
            }
            _ => {}
        };
        for block in &mut document.blocks {
            for span in &mut block.spans {
                match &mut span.style {
                    SpanStyle::Link { url } => apply(url, false),
                    SpanStyle::DocumentLink { target } => apply(target, true),
                    _ => {}
                }
            }
            if let BlockKind::Image {
                link: Some(link), ..
            } = &mut block.kind
            {
                apply(link, false);
            }
        }
        if changed > 0 && !self.replace_document(document) {
            return Err("could not save the changed links".into());
        }
        Ok(changed)
    }

    /// After the file is renamed from `old_stem` to `new_stem`, a title that was only the old
    /// name becomes the new one, in the properties and in the document. A title the author set
    /// is left. Returns whether anything changed.
    pub fn follow_rename(&mut self, old_stem: &str, new_stem: &str) -> bool {
        let mut changed = false;
        if self.metadata.title == old_stem {
            let mut metadata = self.metadata.clone();
            metadata.title = new_stem.to_string();
            self.set_metadata(metadata);
            changed = true;
        }
        if self.editor.document().title == old_stem {
            let mut document = self.editor.document().clone();
            document.title = new_stem.to_string();
            changed |= self.replace_document(document);
        }
        changed
    }

    /// Replaces the whole document with what another machine wrote. Arrivals with no edit between
    /// them join into one undo step, so that a copy left open while writing elsewhere doesn't
    /// keep a whole document for every one of them.
    pub fn follow_document(&mut self, new: Document) -> bool {
        if self.save_failed {
            return false;
        }
        let history = self.history();
        let joined = if self.followed_at == Some(history) {
            self.editor.join_replacing(new.clone())
        } else {
            None
        };
        let saved = match joined {
            Some(op) => {
                self.selection = None;
                let saved = self.record(&op);
                self.valid_caret();
                saved
            }
            None => self.replace_document(new),
        };
        self.followed_at = Some(self.history());
        saved
    }

    /// Replaces a range of a block's text as one undo step. Annotations move as they do for any
    /// edit. Returns whether it happened.
    pub fn replace_text(
        &mut self,
        block_id: BlockId,
        start: usize,
        end: usize,
        text: &str,
    ) -> bool {
        let Some(index) = self.block_index(block_id) else {
            return false;
        };
        let current = &self.document().blocks[index].text;
        if start > end || !current.is_char_boundary(start) || !current.is_char_boundary(end) {
            return false;
        }
        let at = |offset| Cursor { block_id, offset };
        // Worked out on a copy, so that the whole of it is one step.
        let mut draft = Editor::new(self.document().clone());
        if start < end
            && draft
                .delete_selection(&Selection {
                    start: at(start),
                    end: at(end),
                })
                .is_err()
        {
            return false;
        }
        if !text.is_empty() && draft.insert_text(&at(start), text).is_err() {
            return false;
        }
        let saved = self.replace_document(draft.document().clone());
        self.set_caret(at(start + text.len()));
        saved
    }

    pub fn select_word_at(&mut self, at: Cursor) {
        let at = clamp_cursor(self.editor.document(), at);
        let Some(index) = self.block_index(at.block_id) else {
            return;
        };
        let (start, end) = word_at(&self.document().blocks[index].text, at.offset);
        if start == end {
            self.set_caret(at);
            return;
        }
        let cursor = |offset| Cursor {
            block_id: at.block_id,
            offset,
        };
        self.selection = Some(Selection {
            start: cursor(start),
            end: cursor(end),
        });
        self.caret = cursor(end);
    }

    /// The word at `at`, as the cursors either side of it. Both are `at` where there is none.
    pub fn word_range_at(&self, at: Cursor) -> (Cursor, Cursor) {
        let at = clamp_cursor(self.editor.document(), at);
        let Some(index) = self.block_index(at.block_id) else {
            return (at, at);
        };
        let (start, end) = word_at(&self.document().blocks[index].text, at.offset);
        if start == end {
            return (at, at);
        }
        let cursor = |offset| Cursor {
            block_id: at.block_id,
            offset,
        };
        (cursor(start), cursor(end))
    }

    /// Selects whole words from the word `anchor`, which a double click chose, out to the word
    /// at `at`, as dragging from a double click does. The caret follows the pointer.
    pub fn select_words_between(&mut self, anchor: (Cursor, Cursor), at: Cursor) {
        let (word_start, word_end) = self.word_range_at(at);
        let position = |cursor: Cursor| (self.block_index(cursor.block_id), cursor.offset);
        let (start, end) = if position(word_start) < position(anchor.0) {
            (anchor.1, word_start)
        } else {
            (anchor.0, word_end)
        };
        self.selection = (start != end).then_some(Selection { start, end });
        self.caret = end;
    }

    pub fn select_to(&mut self, caret: Cursor) {
        let origin = self
            .selection
            .map_or(self.caret, |selection| selection.start);
        let caret = clamp_cursor(self.editor.document(), caret);
        self.caret = caret;
        self.selection = if origin == caret {
            None
        } else {
            Some(Selection {
                start: origin,
                end: caret,
            })
        };
    }

    /// The selection with its ends in document order, or `None` without one.
    pub fn ordered_selection(&self) -> Option<(Cursor, Cursor)> {
        self.ordered(self.selection?)
    }

    /// Puts the two ends of `selection` in document order.
    pub fn ordered(&self, selection: Selection) -> Option<(Cursor, Cursor)> {
        let start = self.block_index(selection.start.block_id)?;
        let end = self.block_index(selection.end.block_id)?;
        if (start, selection.start.offset) <= (end, selection.end.offset) {
            Some((selection.start, selection.end))
        } else {
            Some((selection.end, selection.start))
        }
    }

    /// Deletes the selected range, leaving the caret where it began.
    ///
    /// Returns `true` when there was nothing to delete or the delete landed in
    /// the database, `false` only on a failed write.
    pub fn delete_selected(&mut self) -> bool {
        let Some(selection) = self.selection.take() else {
            return true;
        };
        let Some((start, end)) = self.ordered(selection) else {
            return true;
        };
        if start == end {
            return true;
        }
        let result = self.editor.delete_selection(&Selection { start, end });
        if self.commit(result) {
            self.caret = start;
            true
        } else {
            false
        }
    }

    pub fn block_index(&self, block_id: crate::model::BlockId) -> Option<usize> {
        self.editor
            .document()
            .blocks
            .iter()
            .position(|block| block.id == block_id)
    }

    // --- editing commands --------------------------------------------------

    pub fn insert(&mut self, text: &str) -> bool {
        self.insert_with(text, |editor, caret| editor.insert_text(caret, text))
    }

    /// [`Session::insert`], as a step of its own in the history. Typing straight after it starts
    /// another step, where it would otherwise join on as the same word.
    pub fn insert_as_step(&mut self, text: &str) -> bool {
        self.insert_with(text, |editor, caret| editor.insert_as_step(caret, text))
    }

    /// [`Session::insert_as_step`], with `spans` on the words, their offsets being within `text`.
    /// It is a step of its own either way, as a paste is.
    fn insert_styled(&mut self, text: &str, spans: &[Span]) -> bool {
        if spans.is_empty() {
            return self.insert_as_step(text);
        }
        self.insert_with(text, |editor, caret| {
            editor.insert_styled(caret, text, spans)
        })
    }

    /// Puts `text` at the caret by way of `edit`, which makes it one edit there.
    fn insert_with(
        &mut self,
        text: &str,
        edit: impl FnOnce(&mut Editor, &Cursor) -> Result<Op, EditError>,
    ) -> bool {
        if self.save_failed {
            return false;
        }
        // Typing over a selection replaces it.
        if self.selection.is_some() && !self.delete_selected() {
            return false;
        }
        // A rule or an image holds no text, so writing on one starts a paragraph after it.
        let caret = self.valid_caret();
        let holds_no_text = self.block_index(caret.block_id).is_some_and(|index| {
            matches!(
                self.document().blocks[index].kind,
                BlockKind::Rule | BlockKind::Image { .. }
            )
        });
        if holds_no_text && !self.split() {
            return false;
        }
        let caret = self.valid_caret();
        match edit(&mut self.editor, &caret) {
            Ok(op) => {
                let saved = self.record(&op);
                self.caret = Cursor {
                    block_id: caret.block_id,
                    offset: caret.offset + text.len(),
                };
                saved
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
                false
            }
        }
    }

    /// Applies a markdown-style prefix at the start of the caret's block (typed
    /// `# `, `## `, `### `, `- `, `* `, `1. `): the prefix is stripped, the
    /// block kind changes, and the caret returns to the start of the block.
    pub fn apply_markdown_shortcut(&mut self) {
        let caret = self.valid_caret();
        if let Some(op) = self.editor.apply_markdown_prefix(caret.block_id) {
            self.record(&op);
            self.caret = Cursor {
                block_id: caret.block_id,
                offset: 0,
            };
        }
    }

    /// Applies inline emphasis the caret just closed (typed `*x*`, `**x**`,
    /// `_x_`, `__x__`): the markers are stripped, the wrapped text takes a
    /// bold/italic span, and the caret lands after the emphasised text.
    pub fn apply_inline_markdown(&mut self) {
        let caret = self.valid_caret();
        let applied = self
            .editor
            .apply_inline_markdown(&caret)
            .or_else(|| self.editor.apply_inline_code(&caret));
        if let Some((op, offset)) = applied {
            self.record(&op);
            self.caret = Cursor {
                block_id: caret.block_id,
                offset,
            };
        }
    }

    /// Applies a markdown link the caret just closed (typed `[text](url)`): the
    /// wrapping is stripped, the anchor text takes a link span, and the caret
    /// lands after the anchor.
    pub fn apply_markdown_link(&mut self) {
        let caret = self.valid_caret();
        let applied = self
            .editor
            .apply_document_link(&caret)
            .or_else(|| self.editor.apply_markdown_link(&caret));
        if let Some((op, offset)) = applied {
            self.record(&op);
            self.caret = Cursor {
                block_id: caret.block_id,
                offset,
            };
        }
    }

    /// Why the caret or selection can't take a document link, if it can't. A span lives
    /// in one block, and the blocks that hold no styled text can't hold a link.
    pub fn document_link_refusal(&self) -> Option<&'static str> {
        if self.save_failed {
            return Some("the last save failed, so retry it first");
        }
        if let Some((start, end)) = self.ordered_selection() {
            if start.block_id != end.block_id {
                return Some("select text within one block to link it");
            }
        }
        let caret = self.caret;
        let block = self
            .document()
            .blocks
            .iter()
            .find(|block| block.id == caret.block_id)?;
        match block.kind {
            BlockKind::Code { .. }
            | BlockKind::Prose
            | BlockKind::Rule
            | BlockKind::Image { .. } => Some("a link can't go in this block"),
            _ => None,
        }
    }

    /// Links to a document: the selected words, or the link the caret is in. With
    /// neither, `title` goes in at the caret as the link's text.
    pub fn link_to_document(&mut self, target: String, title: &str) -> Result<(), &'static str> {
        if let Some(refusal) = self.document_link_refusal() {
            return Err(refusal);
        }
        match self.link_target().filter(|(_, start, end, _)| start < end) {
            Some((block_id, start, end, _)) => {
                let result = self.editor.set_document_link(block_id, start, end, target);
                self.commit(result);
            }
            None => {
                let caret = self.valid_caret();
                let result = self.editor.insert_document_link(&caret, title, target);
                if self.commit(result) {
                    self.selection = None;
                    self.caret = Cursor {
                        block_id: caret.block_id,
                        offset: caret.offset + title.len(),
                    };
                }
            }
        }
        Ok(())
    }

    /// The selected text, with a line break between blocks so that pasting it makes
    /// blocks again. `None` when nothing is selected.
    pub fn selected_text(&self) -> Option<String> {
        self.clipped().map(|clipped| clipped.text)
    }

    /// What's selected, with its formatting, for pasting. An image or a rule is an empty line.
    pub fn clipped(&self) -> Option<Clipped> {
        let (start, end) = self.ordered_selection()?;
        if start == end {
            return None;
        }
        let blocks = &self.document().blocks;
        let first = self.block_index(start.block_id)?;
        let last = self.block_index(end.block_id)?;
        let mut text = String::new();
        let mut spans = Vec::new();
        for (index, block) in blocks[first..=last].iter().enumerate() {
            if index > 0 {
                text.push('\n');
            }
            let from = if index == 0 { start.offset } else { 0 };
            let to = if first + index == last {
                end.offset
            } else {
                block.text.len()
            };
            let base = text.len();
            spans.extend(block.spans.iter().filter_map(|span| {
                let (span_start, span_end) = (span.start.max(from), span.end.min(to));
                (span_start < span_end).then(|| Span {
                    start: span_start - from + base,
                    end: span_end - from + base,
                    style: span.style.clone(),
                })
            }));
            text.push_str(block.text.get(from..to).unwrap_or(""));
        }
        Some(Clipped {
            text,
            spans,
            from: self.link_base(),
        })
    }

    /// The blocks a Tab acts on: those the selection touches, or the caret's. A selection
    /// ending at the very start of a block doesn't reach into it.
    pub fn tabbed_blocks(&self) -> Vec<BlockId> {
        let caret_block = vec![self.caret.block_id];
        let Some((start, end)) = self.ordered_selection().filter(|(start, end)| start != end)
        else {
            return caret_block;
        };
        let (Some(first), Some(last)) = (
            self.block_index(start.block_id),
            self.block_index(end.block_id),
        ) else {
            return caret_block;
        };
        let last = if last > first && end.offset == 0 {
            last - 1
        } else {
            last
        };
        self.document().blocks[first..=last]
            .iter()
            .map(|block| block.id)
            .collect()
    }

    /// Moves the selected list items, or the caret's, a level in or out, if they can go
    /// there.
    pub fn indent_list_item(&mut self, indent: bool) -> bool {
        if self.save_failed {
            return false;
        }
        self.valid_caret();
        let blocks = self.tabbed_blocks();
        match self.editor.indent_list_items(&blocks, indent) {
            Some(op) => {
                self.record(&op);
                true
            }
            None => false,
        }
    }

    /// Joins up a saved history word by word, as typing now does, returning how many entries
    /// there were and now are. Every edit that matters is kept: what goes is stopping points
    /// inside words. What redo can still return to is left as it is.
    ///
    /// This rewrites something irreplaceable, so the new log is first replayed against the old
    /// one, and if they don't agree, or either can't be replayed, nothing changes.
    pub fn compact_history(&mut self) -> Result<(usize, usize), String> {
        if self.save_failed {
            return Err("the last save failed, so retry it first".to_string());
        }
        let failed = |err: DbError| format!("could not read the history: {err}");
        let ops: Vec<Op> = self
            .db
            .load_ops()
            .map_err(failed)?
            .into_iter()
            .map(|(_, op)| op)
            .collect();
        let cursor = self.db.cursor().map_err(failed)?.min(ops.len());
        let (undo, redo) = ops.split_at(cursor);
        let joined = crate::edit::ops::compact(undo);
        if joined.len() == undo.len() {
            return Ok((undo.len(), undo.len()));
        }
        let head = self.editor.document().clone();
        let agrees = std::panic::catch_unwind(|| histories_agree(&head, undo, &joined));
        if !matches!(agrees, Ok(true)) {
            return Err("the history doesn't replay cleanly, so it was left as it is".to_string());
        }
        let mut all = joined;
        let new_cursor = all.len();
        all.extend_from_slice(redo);
        self.db
            .replace_log(&all, new_cursor)
            .map_err(|err| format!("could not save the history: {err}"))?;
        self.editor = Editor::with_cursor(head, all, new_cursor);
        Ok((undo.len(), new_cursor))
    }

    /// The start of the first heading with exactly `text`.
    pub fn heading_cursor(&self, text: &str) -> Option<Cursor> {
        self.document()
            .blocks
            .iter()
            .find(|block| matches!(block.kind, BlockKind::Heading { .. }) && block.text == text)
            .map(|block| Cursor {
                block_id: block.id,
                offset: 0,
            })
    }

    /// What a relative link in this document is relative to: its file, or the Markdown
    /// file it was imported from.
    pub fn link_base(&self) -> PathBuf {
        self.path()
            .or(self.source_path.as_deref())
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    /// Deletes `selection` and parks the caret at `caret_after`.
    pub fn delete_range(&mut self, selection: Selection, caret_after: Cursor) -> bool {
        let result = self.editor.delete_selection(&selection);
        if self.commit(result) {
            self.caret = caret_after;
            true
        } else {
            false
        }
    }

    pub fn backspace(&mut self) -> bool {
        if self.save_failed {
            return false;
        }
        if self.selection.is_some() {
            return self.delete_selected();
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return false;
        };

        let previous = {
            let document = self.editor.document();
            prev_boundary(&document.blocks[index].text, caret.offset)
        };
        if let Some(offset) = previous {
            let target = Cursor {
                block_id: caret.block_id,
                offset,
            };
            return self.delete_range(
                Selection {
                    start: target,
                    end: caret,
                },
                target,
            );
        }
        if index == 0 {
            return false;
        }

        // At the start of a block: join it to the previous one.
        let (previous_id, previous_len) = {
            let document = self.editor.document();
            let previous = &document.blocks[index - 1];
            (previous.id, previous.text.len())
        };
        let at_end = Cursor {
            block_id: previous_id,
            offset: previous_len,
        };
        let result = self.editor.merge_with_next(&at_end);
        if self.commit(result) {
            self.caret = at_end;
            true
        } else {
            false
        }
    }

    pub fn delete_forward(&mut self) -> bool {
        if self.save_failed {
            return false;
        }
        if self.selection.is_some() {
            return self.delete_selected();
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return false;
        };

        let next = {
            let document = self.editor.document();
            next_boundary(&document.blocks[index].text, caret.offset)
        };
        if let Some(offset) = next {
            let end = Cursor {
                block_id: caret.block_id,
                offset,
            };
            return self.delete_range(Selection { start: caret, end }, caret);
        }
        if index + 1 >= self.editor.document().blocks.len() {
            return false;
        }
        // At the end of a block: pull the next block's text up.
        let result = self.editor.merge_with_next(&caret);
        self.commit(result)
    }

    /// Shift-Enter: a line in a verbatim block, however blank the one before it. In
    /// any other block it is just Enter.
    pub fn line_break(&mut self) -> bool {
        let caret = self.valid_caret();
        let verbatim = self
            .block_index(caret.block_id)
            .is_some_and(|index| self.document().blocks[index].kind.is_verbatim());
        if verbatim {
            self.insert("\n")
        } else {
            self.split()
        }
    }

    pub fn split(&mut self) -> bool {
        if self.save_failed {
            return false;
        }
        if self.selection.is_some() && !self.delete_selected() {
            return false;
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return false;
        };
        if self.editor.enter_adds_line(&caret) {
            return self.insert("\n");
        }
        let blocks_before = self.editor.document().blocks.len();
        let result = self.editor.split_paragraph(&caret);
        if !self.commit(result) {
            return false;
        }
        // Enter can change the block instead of adding one, and then the caret stays.
        if self.editor.document().blocks.len() == blocks_before {
            self.caret = Cursor {
                block_id: caret.block_id,
                offset: 0,
            };
            return true;
        }
        if let Some(next) = self.editor.document().blocks.get(index + 1) {
            self.caret = Cursor {
                block_id: next.id,
                offset: 0,
            };
            true
        } else {
            false
        }
    }

    /// Inserts an image block after the caret and moves the caret into it.
    pub fn insert_image(&mut self, path: String) -> bool {
        if self.save_failed {
            return false;
        }
        if self.selection.is_some() && !self.delete_selected() {
            return false;
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return false;
        };
        let result = self.editor.insert_image_block(&caret, path);
        if !self.commit(result) {
            return false;
        }
        if let Some(next) = self.editor.document().blocks.get(index + 1) {
            self.caret = Cursor {
                block_id: next.id,
                offset: 0,
            };
            true
        } else {
            false
        }
    }

    /// Moves an image across its line. Anything else, or no change, does nothing.
    pub fn set_image_align(&mut self, block_id: BlockId, align: ImageAlign) -> bool {
        let Some(index) = self.block_index(block_id) else {
            return false;
        };
        let BlockKind::Image {
            path,
            link,
            align: current,
        } = self.document().blocks[index].kind.clone()
        else {
            return false;
        };
        if current == align {
            return false;
        }
        self.set_block_kind(block_id, BlockKind::Image { path, link, align })
    }

    /// After a save as, says each image's path again for where the document is now, having been
    /// at `from` until then. `root` is the notebook it's in now, if any. An image that can't be
    /// found keeps what it has, since there's nothing to say about it.
    pub fn rehome_images(&mut self, from: Option<&Path>, root: Option<&Path>) {
        let Some(document) = self.path.clone() else {
            return;
        };
        let images: Vec<(BlockId, String, Option<String>, ImageAlign)> = self
            .document()
            .blocks
            .iter()
            .filter_map(|block| match &block.kind {
                BlockKind::Image { path, link, align } => {
                    Some((block.id, path.clone(), link.clone(), *align))
                }
                _ => None,
            })
            .collect();
        for (id, stored, link, align) in images {
            let file = if Path::new(&stored).is_absolute() {
                PathBuf::from(&stored)
            } else {
                match whole_image_path(&stored, from) {
                    Some(file) => file,
                    None => continue,
                }
            };
            if !file.exists() {
                continue;
            }
            let path = stored_image_path(&file, &document, root);
            if path != stored {
                let kind = BlockKind::Image { path, link, align };
                let result = self.editor.set_block_kind(id, kind);
                self.commit(result);
            }
        }
    }

    /// After a save as, says each relative link again for where the document is now, having been
    /// at `from` until then, as one undo step. A link to the document itself goes on leading to
    /// it. A web link or `#Heading` depends on nothing of the kind and stays, and a document that
    /// had no file has no way to say where one led. A link to anything at all is only taken for a
    /// path if what it names was there, as what looks like a path may be an address someone
    /// wrote, while a link to a document always names one, or one to come.
    pub fn rehome_links(&mut self, from: Option<&Path>) {
        let (Some(from), Some(to)) = (from, self.path.clone()) else {
            return;
        };
        let selection = self.selection;
        let rewritten = self.rewrite_links_by_kind(|link, to_document| {
            (to_document || names_something(link, from))
                .then(|| own_target_after_save_as(link, from, &to))
        });
        match rewritten {
            Ok(_) => self.selection = selection,
            Err(err) => self.last_error = Some(err),
        }
    }

    /// Sets a link span over `start..end`, replacing any link already there; an
    /// empty `url` removes it.
    pub fn set_link(&mut self, block_id: BlockId, start: usize, end: usize, url: String) -> bool {
        if self.save_failed {
            return false;
        }
        let result = self.editor.set_link(block_id, start, end, url);
        self.commit(result)
    }

    /// Sets a link's words and address together, as one step. Empty words show the address. An
    /// empty address takes the link away, and if the words are empty too, they stay as they were.
    /// Only the words that differ are replaced, so that notes on the rest stay where they are.
    pub fn edit_link(
        &mut self,
        block_id: BlockId,
        start: usize,
        end: usize,
        words: &str,
        url: &str,
    ) -> bool {
        if self.save_failed {
            return false;
        }
        let Some(index) = self.block_index(block_id) else {
            return false;
        };
        let block = &self.document().blocks[index];
        let fits = start < end
            && end <= block.text.len()
            && block.text.is_char_boundary(start)
            && block.text.is_char_boundary(end);
        if !fits {
            return false;
        }
        let url = url.trim();
        let current = &block.text[start..end];
        let words = if !words.trim().is_empty() {
            words
        } else if !url.is_empty() {
            url
        } else {
            current
        };
        if words == current {
            return self.set_link(block_id, start, end, url.to_string());
        }
        let spans = spans_after_replacing(&block.spans, start, end, words.len());
        let prefix: usize = current
            .chars()
            .zip(words.chars())
            .take_while(|(one, other)| one == other)
            .map(|(one, _)| one.len_utf8())
            .sum();
        let suffix: usize = current[prefix..]
            .chars()
            .rev()
            .zip(words[prefix..].chars().rev())
            .take_while(|(one, other)| one == other)
            .map(|(one, _)| one.len_utf8())
            .sum();
        let at = |offset| Cursor { block_id, offset };
        let (changed_start, changed_end) = (start + prefix, end - suffix);
        let added = &words[prefix..words.len() - suffix];
        // Worked out on a copy, so that the whole of it is one step.
        let mut draft = Editor::new(self.document().clone());
        if changed_start < changed_end
            && draft
                .delete_selection(&Selection {
                    start: at(changed_start),
                    end: at(changed_end),
                })
                .is_err()
        {
            return false;
        }
        if !added.is_empty() && draft.insert_text(&at(changed_start), added).is_err() {
            return false;
        }
        if draft.set_spans_of(block_id, spans).is_err() {
            return false;
        }
        let new_end = start + words.len();
        if !url.is_empty()
            && draft
                .set_link(block_id, start, new_end, url.to_string())
                .is_err()
        {
            return false;
        }
        let saved = self.replace_document(draft.document().clone());
        self.set_caret(at(new_end));
        saved
    }

    /// The link under `at`, of any kind, including a bare URL, which has no span.
    pub fn link_under(&self, at: Cursor) -> Option<LinkUnder> {
        let block = &self.document().blocks[self.block_index(at.block_id)?];
        let under = |start: usize, end: usize| start <= at.offset && at.offset < end;
        let stored = block.spans.iter().find_map(|span| {
            let (url, document) = match &span.style {
                SpanStyle::Link { url } => (url, false),
                SpanStyle::DocumentLink { target } => (target, true),
                _ => return None,
            };
            under(span.start, span.end).then(|| LinkUnder {
                block_id: block.id,
                start: span.start,
                end: span.end,
                url: url.clone(),
                document,
            })
        });
        stored.or_else(|| {
            bare_link_ranges(block)
                .into_iter()
                .find(|(start, end)| under(*start, *end))
                .map(|(start, end)| LinkUnder {
                    block_id: block.id,
                    start,
                    end,
                    url: block.text[start..end].to_string(),
                    document: false,
                })
        })
    }

    /// The range C-k should link — the current selection, or the link under the
    /// caret when nothing is selected — plus that link's current URL, if any,
    /// so the prompt can pre-fill it for editing.
    pub fn link_target(&self) -> Option<(BlockId, usize, usize, Option<String>)> {
        if let Some((start, end)) = self.ordered_selection() {
            if start.block_id != end.block_id {
                return None; // a span lives in one block
            }
            let url = self
                .link_span_at(start.block_id, start.offset, end.offset)
                .map(|(_, _, url)| url);
            return Some((start.block_id, start.offset, end.offset, url));
        }
        let caret = self.caret;
        let (start, end, url) = self.link_span_at(caret.block_id, caret.offset, caret.offset)?;
        Some((caret.block_id, start, end, Some(url)))
    }

    /// The stored link span covering `lo..hi` (`lo == hi` means "the link
    /// containing the caret"), returning `(start, end, url)`.
    pub fn link_span_at(
        &self,
        block_id: BlockId,
        lo: usize,
        hi: usize,
    ) -> Option<(usize, usize, String)> {
        let block = self
            .editor
            .document()
            .blocks
            .iter()
            .find(|block| block.id == block_id)?;
        block
            .spans
            .iter()
            .filter_map(|span| match &span.style {
                SpanStyle::Link { url } | SpanStyle::DocumentLink { target: url }
                    if span.start <= lo && hi <= span.end =>
                {
                    Some((span.start, span.end, url.clone()))
                }
                _ => None,
            })
            .next()
    }

    /// Changes `block_id`'s kind, persisted as one undoable edit.
    ///
    /// This is the authoring path for headings and list items; without it every
    /// block a user can create is a paragraph.
    pub fn set_block_kind(&mut self, block_id: BlockId, kind: BlockKind) -> bool {
        if self.save_failed {
            return false;
        }
        // Changing what kind of list an item is in leaves it at its level.
        let existing =
            self.block_index(block_id)
                .and_then(|index| match self.document().blocks[index].kind {
                    BlockKind::ListItem { depth, .. } => Some(depth),
                    _ => None,
                });
        let kind = match (kind, existing) {
            (BlockKind::ListItem { ordered, .. }, Some(depth)) => {
                BlockKind::ListItem { ordered, depth }
            }
            (kind, _) => kind,
        };
        let result = self.editor.set_block_kind(block_id, kind);
        self.commit(result)
    }

    /// Inserts pasted text one line per block: each newline becomes a
    /// paragraph break, so the block model keeps its one-paragraph invariant.
    /// [`Session::paste`], keeping the formatting of `clipped`, what kame last copied, if that's
    /// what's being pasted. The clipboard itself is the system's and holds only text, so anything
    /// copied elsewhere since will differ, and goes in as text.
    pub fn paste_clipped(&mut self, text: &str, clipped: Option<&Clipped>) -> bool {
        let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
        let Some(clipped) = clipped.filter(|clipped| clipped.text == normalised) else {
            return self.paste(text);
        };
        if self.save_failed {
            return false;
        }
        if self.selection.is_some() && !self.delete_selected() {
            return false;
        }
        // A relative link has to lead to the same place from here.
        let here = self.link_base();
        let moved: Vec<Span> = clipped
            .spans
            .iter()
            .map(|span| {
                let style = match &span.style {
                    SpanStyle::Link { url } => SpanStyle::Link {
                        url: retargeted(url, &clipped.from, &here),
                    },
                    SpanStyle::DocumentLink { target } => SpanStyle::DocumentLink {
                        target: retargeted(target, &clipped.from, &here),
                    },
                    other => other.clone(),
                };
                Span {
                    start: span.start,
                    end: span.end,
                    style,
                }
            })
            .collect();
        let caret = self.valid_caret();
        let verbatim = self
            .block_index(caret.block_id)
            .is_some_and(|index| self.document().blocks[index].kind.is_verbatim());
        if verbatim {
            return self.insert_styled(&normalised, &moved);
        }
        let mut changed = false;
        let mut base = 0;
        for (index, line) in normalised.split('\n').enumerate() {
            if index > 0 {
                changed |= self.split();
            }
            if !line.is_empty() {
                let end = base + line.len();
                let spans: Vec<Span> = moved
                    .iter()
                    .filter_map(|span| {
                        let (start, stop) = (span.start.max(base), span.end.min(end));
                        (start < stop).then(|| Span {
                            start: start - base,
                            end: stop - base,
                            style: span.style.clone(),
                        })
                    })
                    .collect();
                changed |= self.insert_styled(line, &spans);
            }
            base += line.len() + 1;
        }
        changed
    }

    pub fn paste(&mut self, text: &str) -> bool {
        if self.save_failed {
            return false;
        }
        if self.selection.is_some() && !self.delete_selected() {
            return false;
        }
        let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
        // A verbatim block holds line breaks, so what is pasted into one stays in it.
        let caret = self.valid_caret();
        let verbatim = self
            .block_index(caret.block_id)
            .is_some_and(|index| self.document().blocks[index].kind.is_verbatim());
        if verbatim {
            return self.insert_as_step(&normalised);
        }
        let mut changed = false;
        for (index, line) in normalised.split('\n').enumerate() {
            if index > 0 {
                changed |= self.split();
            }
            if !line.is_empty() {
                changed |= self.insert_as_step(line);
            }
        }
        changed
    }

    /// Deletes the word before the caret. A selection goes instead, as it does for
    /// Backspace, and at the start of a block this joins it to the one before.
    pub fn delete_word_back(&mut self) -> bool {
        if self.save_failed {
            return false;
        }
        if self.selection.is_some() {
            return self.delete_selected();
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return false;
        };
        let Some(offset) = prev_word(&self.document().blocks[index].text, caret.offset) else {
            return self.backspace();
        };
        let target = Cursor {
            block_id: caret.block_id,
            offset,
        };
        self.delete_range(
            Selection {
                start: target,
                end: caret,
            },
            target,
        )
    }

    /// The very start of the first block, or the very end of the last.
    pub fn document_edge(&self, end: bool) -> Option<Cursor> {
        let blocks = &self.document().blocks;
        if end {
            blocks.last().map(|block| Cursor {
                block_id: block.id,
                offset: block.text.len(),
            })
        } else {
            blocks.first().map(|block| Cursor {
                block_id: block.id,
                offset: 0,
            })
        }
    }

    pub fn undo(&mut self) -> bool {
        if self.save_failed {
            return false;
        }
        let Some(op) = self.editor.undo() else {
            self.report_refused("undo");
            return false;
        };
        self.selection = None;
        self.save_state();
        self.place_caret_from(&op);
        true
    }

    pub fn redo(&mut self) -> bool {
        if self.save_failed {
            return false;
        }
        let Some(op) = self.editor.redo() else {
            self.report_refused("redo");
            return false;
        };
        self.selection = None;
        self.save_state();
        self.place_caret_from(&op);
        true
    }

    /// Says so if an undo or redo was refused because this step of the history no longer fits the
    /// document, which older files can do. Nothing changed, and editing carries on.
    fn report_refused(&mut self, which: &str) {
        if self.editor.take_refused() {
            self.last_error = Some(format!(
                "could not {which} this step: it no longer fits the document. Nothing changed, \
                 and any snapshots still have earlier versions"
            ));
        }
    }

    /// Moves the caret to the position implied by an applied undo/redo op.
    ///
    /// The editor clamps a stale offset but does not move the caret, so an
    /// undo of text typed mid-block used to leave the caret past the erased
    /// span — the next insert then landed in the wrong place.
    pub fn place_caret_from(&mut self, op: &Op) {
        if let Some(caret) = caret_for_op(self.editor.document(), op) {
            self.set_caret(caret);
        } else {
            self.caret = clamp_cursor(self.editor.document(), self.caret);
        }
    }

    /// Anchors a new annotation of `kind` over the selection, or over the word
    /// under a bare caret, and persists it.
    ///
    /// Returns the new id so the caller can open its popover. `None` means the
    /// write failed (the session is now read-only) or the caret names no block.
    pub fn add_annotation(&mut self, kind: AnnotationKind) -> Option<AnnotationId> {
        if self.save_failed {
            return None;
        }
        let anchor = self.annotation_anchor(kind)?;
        let snippet = anchor
            .byte_range()
            .and_then(|(start, end)| {
                self.editor
                    .document()
                    .blocks
                    .iter()
                    .find(|block| block.id == anchor.block_id())
                    .map(|block| {
                        &block.text[start.min(block.text.len())..end.min(block.text.len())]
                    })
            })
            .unwrap_or("");
        let body = default_annotation_body(&kind, snippet);
        match self.editor.add_annotation(kind, anchor, &body) {
            Ok(op) => {
                let id = match &op {
                    Op::AddAnnotation { annotation } => annotation.id,
                    _ => return None,
                };
                if self.record(&op) {
                    Some(id)
                } else {
                    None
                }
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
                None
            }
        }
    }

    /// Takes away the marginal `id` if nothing has been done since it was added and it still says
    /// what it was added with. The add leaves the history as well, so that nothing is left to
    /// redo, unless words written in it and undone are still waiting for a redo, when the add is
    /// undone instead. A marginal that was written in, or anything else, is left, and the caret
    /// and selection stay as they are. So is a marginal the file wouldn't let go, with the failed
    /// write reported. Returns whether it went.
    pub fn discard_unwritten_marginal(&mut self, id: AnnotationId) -> bool {
        let unwritten = match self.editor.applied_ops().last() {
            Some(Op::AddAnnotation { annotation }) => {
                annotation.id == id
                    && annotation.kind == AnnotationKind::Marginal
                    && self
                        .editor
                        .document()
                        .annotations
                        .iter()
                        .any(|now| now == annotation)
            }
            _ => false,
        };
        if !unwritten || self.save_failed {
            return false;
        }
        let (caret, selection) = (self.caret, self.selection);
        if self.history().1 > 0 {
            if !self.undo() {
                return false;
            }
            self.caret = caret;
            self.selection = selection;
            return true;
        }
        let Some(op) = self.editor.forget_last_edit() else {
            self.report_refused("undo");
            return false;
        };
        self.check_file();
        if !self.replaced && !self.newer {
            let forgotten = self
                .db
                .forget_last_edit(self.editor.document(), self.editor.cursor());
            match forgotten {
                Ok(()) => self.clear_save_error(),
                Err(err) => {
                    // The file still has the add, so memory goes back to having it too: the log
                    // and the redo stack have to agree, for what's typed next to join the right
                    // step. Closing the marginal again can try again.
                    self.editor.put_back(op);
                    self.mark_save_failed(err);
                    return false;
                }
            }
        }
        true
    }

    /// Replaces a note's body and persists it as one undoable edit. A body the note already has
    /// is no edit, and nothing is logged for it.
    ///
    /// Returns whether the write landed; a refused edit leaves `last_error` set
    /// so the popover's caller can see why nothing was saved.
    pub fn set_annotation_body(&mut self, id: AnnotationId, body: &str) -> bool {
        if self.save_failed {
            return false;
        }
        let annotation = self
            .editor
            .document()
            .annotations
            .iter()
            .find(|annotation| annotation.id == id);
        // An emptied marginalia note is removed rather than kept as an empty
        // margin entry.
        let is_empty_marginal = body.trim().is_empty()
            && annotation.is_some_and(|annotation| annotation.kind == AnnotationKind::Marginal);
        if !is_empty_marginal && annotation.is_some_and(|annotation| annotation.body == body) {
            return true;
        }
        let result = if is_empty_marginal {
            self.editor.remove_annotation(id)
        } else {
            self.editor.set_annotation_body(id, body)
        };
        self.commit(result)
    }

    /// Makes each citation of `old` a citation of `new`: in blocks, footnotes and marginals, where
    /// `[old]` counts as one, and never in a note. The whole rename is one undo step. Only the key
    /// changes, and spans, anchors, the caret and the selection are moved across it by hand so
    /// that whatever covered a key still covers it, and a note on exactly the key stays on it.
    /// Returns how many citations changed; 0 and no change when nothing did, when `new` is
    /// the same or not a key, or when the document can't be written.
    pub fn rename_citation(&mut self, old: &str, new: &str) -> usize {
        if old == new || !valid_key(new) || self.save_failed || self.newer || self.replaced {
            return 0;
        }
        let mut document = self.document().clone();
        let mut keys_in: HashMap<BlockId, Vec<(usize, usize)>> = HashMap::new();
        let mut changed = 0;
        for block in &mut document.blocks {
            let keys: Vec<(usize, usize)> = block_citation_ranges(block)
                .into_iter()
                .filter(|range| key_of(&block.text, *range) == old)
                .map(|(start, end)| (start + 1, end - 1))
                .collect();
            if keys.is_empty() {
                continue;
            }
            changed += keys.len();
            let mut text = String::with_capacity(block.text.len());
            let mut from = 0;
            for (start, end) in &keys {
                text.push_str(&block.text[from..*start]);
                text.push_str(new);
                from = *end;
            }
            text.push_str(&block.text[from..]);
            for span in &mut block.spans {
                span.start = offset_after_rename(span.start, &keys, new.len(), false);
                span.end = offset_after_rename(span.end, &keys, new.len(), true);
            }
            block.text = text;
            keys_in.insert(block.id, keys);
        }
        for annotation in &mut document.annotations {
            if let Anchor::Range {
                block_id,
                start,
                end,
            } = &mut annotation.anchor
            {
                if let Some(keys) = keys_in.get(block_id) {
                    // An anchor at one point stays at one point, where a caret there would go.
                    let at_one_point = *start == *end;
                    *end = offset_after_rename(*end, keys, new.len(), true);
                    *start = if at_one_point {
                        *end
                    } else {
                        offset_after_rename(*start, keys, new.len(), false)
                    };
                }
            }
            if annotation.kind != AnnotationKind::Note {
                let count = citation_ranges(&annotation.body)
                    .into_iter()
                    .filter(|range| key_of(&annotation.body, *range) == old)
                    .count();
                if count > 0 {
                    annotation.body = rewrite_citation(&annotation.body, old, new);
                    changed += count;
                }
            }
        }
        if changed == 0 {
            return 0;
        }
        let moved = |cursor: Cursor| Cursor {
            block_id: cursor.block_id,
            offset: keys_in.get(&cursor.block_id).map_or(cursor.offset, |keys| {
                offset_after_rename(cursor.offset, keys, new.len(), true)
            }),
        };
        let selection = self.selection.map(|selection| {
            let mut after = Selection {
                start: moved(selection.start),
                end: moved(selection.end),
            };
            // Both ends went to the end of the one key they were in, so it covers that key, as
            // a span inside one comes to, with the moving end still the moving end.
            if after.start == after.end && selection.start != selection.end {
                let key_start = Cursor {
                    block_id: after.end.block_id,
                    offset: after.end.offset - new.len(),
                };
                if selection.start.offset < selection.end.offset {
                    after.start = key_start;
                } else {
                    after.end = key_start;
                }
            }
            after
        });
        let caret = match (self.selection, selection) {
            (Some(before), Some(after)) if before.end == self.caret => after.end,
            _ => moved(self.caret),
        };
        if !self.replace_document(document) {
            return 0;
        }
        self.caret = caret;
        self.selection = selection;
        self.valid_caret();
        changed
    }

    /// Removes an annotation (a highlight, marginal or footnote) outright,
    /// returning whether the removal was persisted as one undoable edit.
    pub fn remove_annotation(&mut self, id: AnnotationId) -> bool {
        if self.save_failed {
            return false;
        }
        let result = self.editor.remove_annotation(id);
        self.commit(result)
    }

    /// The anchor for a new annotation of `kind`: marginalia anchor to a whole
    /// block, notes and footnotes to the selection (clamped to its first block,
    /// since an anchor lives in one block) or the word under the caret.
    pub fn annotation_anchor(&self, kind: AnnotationKind) -> Option<Anchor> {
        let document = self.editor.document();
        let block_id = if let Some((start, _)) = self.ordered_selection() {
            start.block_id
        } else {
            self.caret.block_id
        };
        document.blocks.iter().find(|block| block.id == block_id)?;

        if kind == AnnotationKind::Marginal {
            return Some(Anchor::block(block_id));
        }

        if let Some((start, end)) = self.ordered_selection() {
            let block = document
                .blocks
                .iter()
                .find(|block| block.id == start.block_id)?;
            let end = if end.block_id == start.block_id {
                end.offset
            } else {
                block.text.len()
            };
            let start_offset = start.offset.min(end);
            // A footnote's number goes at the end, which must not be inside a word. A highlight
            // keeps exactly what was selected.
            let (start_offset, end) = if kind == AnnotationKind::Footnote {
                widen_to_words(&block.text, start_offset, end)
            } else {
                (start_offset, end)
            };
            return Some(Anchor::range(block.id, start_offset, end));
        }

        let caret = self.caret;
        let block = document
            .blocks
            .iter()
            .find(|block| block.id == caret.block_id)?;
        let (start, end) = word_at(&block.text, caret.offset);
        Some(Anchor::range(block.id, start, end))
    }

    pub fn move_left(&mut self, select: bool) {
        // An arrow collapses a selection to its edge before moving — unless it
        // is extending the selection, which keeps the fixed edge and moves the
        // caret end.
        if !select {
            if let Some((start, _)) = self.ordered_selection() {
                self.caret = start;
                self.selection = None;
                return;
            }
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return;
        };
        let target = {
            let document = self.editor.document();
            let block = &document.blocks[index];
            match prev_boundary(&block.text, caret.offset) {
                Some(offset) => Cursor {
                    block_id: block.id,
                    offset,
                },
                None if index > 0 => {
                    let previous = &document.blocks[index - 1];
                    Cursor {
                        block_id: previous.id,
                        offset: previous.text.len(),
                    }
                }
                None => caret,
            }
        };
        if select {
            self.select_to(target);
        } else {
            self.caret = target;
        }
    }

    pub fn move_right(&mut self, select: bool) {
        if !select {
            if let Some((_, end)) = self.ordered_selection() {
                self.caret = end;
                self.selection = None;
                return;
            }
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return;
        };
        let target = {
            let document = self.editor.document();
            let block = &document.blocks[index];
            match next_boundary(&block.text, caret.offset) {
                Some(offset) => Cursor {
                    block_id: block.id,
                    offset,
                },
                None => match document.blocks.get(index + 1) {
                    Some(next) => Cursor {
                        block_id: next.id,
                        offset: 0,
                    },
                    None => caret,
                },
            }
        };
        if select {
            self.select_to(target);
        } else {
            self.caret = target;
        }
    }

    /// Ctrl-arrow: move left by a whole word. A caret inside a word jumps to
    /// its start; one between words jumps to the previous word's start. At the
    /// start of a block the caret moves to the end of the previous block.
    pub fn move_left_word(&mut self, select: bool) {
        if !select {
            if let Some((start, _)) = self.ordered_selection() {
                self.caret = start;
                self.selection = None;
                return;
            }
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return;
        };
        let target = {
            let document = self.editor.document();
            let block = &document.blocks[index];
            match prev_word(&block.text, caret.offset) {
                Some(offset) => Cursor {
                    block_id: block.id,
                    offset,
                },
                None if index > 0 => {
                    let previous = &document.blocks[index - 1];
                    Cursor {
                        block_id: previous.id,
                        offset: previous.text.len(),
                    }
                }
                None => caret,
            }
        };
        if select {
            self.select_to(target);
        } else {
            self.caret = target;
        }
    }

    /// Ctrl-arrow: move right by a whole word, landing on the next word's
    /// start, or after the block's last word and any punctuation behind it. From
    /// there the caret moves to the start of the next block.
    pub fn move_right_word(&mut self, select: bool) {
        if !select {
            if let Some((_, end)) = self.ordered_selection() {
                self.caret = end;
                self.selection = None;
                return;
            }
        }
        let caret = self.valid_caret();
        let Some(index) = self.block_index(caret.block_id) else {
            return;
        };
        let target = {
            let document = self.editor.document();
            let block = &document.blocks[index];
            match next_word(&block.text, caret.offset) {
                Some(offset) => Cursor {
                    block_id: block.id,
                    offset,
                },
                None => match document.blocks.get(index + 1) {
                    Some(next) => Cursor {
                        block_id: next.id,
                        offset: 0,
                    },
                    None => caret,
                },
            }
        };
        if select {
            self.select_to(target);
        } else {
            self.caret = target;
        }
    }
}

pub type AppResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// What kame copied: the words as they went on the system's clipboard, and their formatting, with
/// offsets into those words.
#[derive(Clone, Debug, PartialEq)]
pub struct Clipped {
    pub text: String,
    pub spans: Vec<Span>,
    /// What its relative links are relative to.
    pub from: PathBuf,
}

/// A link in a block, stored or bare.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkUnder {
    pub block_id: BlockId,
    pub start: usize,
    pub end: usize,
    pub url: String,
    /// Whether it's a link to a document or a heading, which kame follows itself.
    pub document: bool,
}

/// `spans` once `start..end` has been replaced by `new_len` bytes, with any link overlapping it gone.
/// Formatting over all of the old words is over all of the new ones. Formatting over only some of
/// them keeps whatever part lies outside.
fn spans_after_replacing(spans: &[Span], start: usize, end: usize, new_len: usize) -> Vec<Span> {
    let moved = |offset: usize| offset - (end - start) + new_len;
    spans
        .iter()
        .filter_map(|span| {
            let overlaps = span.start < end && start < span.end;
            let is_link = matches!(
                span.style,
                SpanStyle::Link { .. } | SpanStyle::DocumentLink { .. }
            );
            let (from, to) = if span.end <= start {
                (span.start, span.end)
            } else if span.start >= end {
                (moved(span.start), moved(span.end))
            } else if is_link && overlaps {
                return None;
            } else if span.start <= start && span.end >= end {
                (span.start, moved(span.end))
            } else if span.start < start {
                (span.start, start)
            } else if span.end > end {
                (start + new_len, moved(span.end))
            } else {
                return None;
            };
            Some(Span {
                start: from,
                end: to,
                style: span.style.clone(),
            })
        })
        .collect()
}

/// Readable and writable by the owner alone.
pub const PRIVATE_MODE: u32 = 0o600;

/// Where `offset` goes when each key in `keys` (byte ranges, in order) becomes `new_len` bytes. An
/// offset at or before a key's start stays, one at or after its end moves with the text, and one
/// inside goes to the key's start if it begins a range, else to the key's new end.
fn offset_after_rename(
    offset: usize,
    keys: &[(usize, usize)],
    new_len: usize,
    ends: bool,
) -> usize {
    let mut shift: isize = 0;
    for (start, end) in keys {
        if offset <= *start {
            break;
        }
        if offset >= *end {
            shift += new_len as isize - (end - start) as isize;
        } else {
            let start = (*start as isize + shift) as usize;
            return if ends { start + new_len } else { start };
        }
    }
    (offset as isize + shift) as usize
}

/// Caret position implied by an applied op: the start of erased text, the end
/// of inserted text, or the start of a block that now exists.
///
/// `Op::Batch` members are scanned in order, skipping members whose block did
/// not survive the operation (undoing a split returns a batch whose first
/// member deletes the block the split created; the text member that follows
/// carries the split point).
pub fn caret_for_op(document: &Document, op: &Op) -> Option<Cursor> {
    let existing = |block_id: crate::model::BlockId| {
        document
            .blocks
            .iter()
            .any(|block| block.id == block_id)
            .then_some(block_id)
    };
    match op {
        Op::InsertText {
            block_id,
            offset,
            text,
        } => existing(*block_id).map(|block_id| Cursor {
            block_id,
            offset: offset + text.len(),
        }),
        Op::DeleteText {
            block_id, start, ..
        } => existing(*block_id).map(|block_id| Cursor {
            block_id,
            offset: *start,
        }),
        Op::InsertBlock { block, .. } | Op::DeleteBlock { block, .. } => {
            existing(block.id).map(|block_id| Cursor {
                block_id,
                offset: 0,
            })
        }
        Op::Batch { ops } => ops.iter().find_map(|member| caret_for_op(document, member)),
        _ => None,
    }
}

/// Clamps a caret to a block that exists and to a character boundary inside it.
pub fn clamp_cursor(document: &Document, cursor: Cursor) -> Cursor {
    match document
        .blocks
        .iter()
        .find(|block| block.id == cursor.block_id)
    {
        Some(block) => {
            let mut offset = cursor.offset.min(block.text.len());
            while offset > 0 && !block.text.is_char_boundary(offset) {
                offset -= 1;
            }
            Cursor {
                block_id: block.id,
                offset,
            }
        }
        None => document
            .blocks
            .first()
            .map(|block| Cursor {
                block_id: block.id,
                offset: 0,
            })
            .unwrap_or(cursor),
    }
}

/// Default body for a newly anchored annotation.
///
/// A note starts empty — it is a bare highlight, and its comment is the
/// author's to add in the popover. Marginalia and footnotes quote the anchored
/// words as a starting point (they are painted in place rather than hidden
/// behind a popover).
pub fn default_annotation_body(kind: &AnnotationKind, snippet: &str) -> String {
    let snippet = shorten(snippet.trim(), 48);
    if snippet.is_empty() {
        return match kind {
            AnnotationKind::Marginal => "New marginal note.".to_string(),
            AnnotationKind::Note => String::new(),
            AnnotationKind::Footnote => "New footnote.".to_string(),
        };
    }
    match kind {
        AnnotationKind::Marginal => format!("Marginal note on “{snippet}”"),
        AnnotationKind::Note => String::new(),
        AnnotationKind::Footnote => format!("Footnote on “{snippet}”"),
    }
}

/// Truncates to `max` characters (not bytes) with an ellipsis.
pub fn shorten(text: &str, max: usize) -> String {
    let mut out = String::new();
    for (index, character) in text.chars().enumerate() {
        if index == max {
            out.push('…');
            break;
        }
        out.push(character);
    }
    out
}

/// Metadata for a file that has none stored: its title if it was ever given one,
/// and its own date as the created date.
pub fn default_metadata(title: &str, path: &Path) -> Metadata {
    let created = std::fs::metadata(path)
        .ok()
        .and_then(|file| file.created().or_else(|_| file.modified()).ok())
        .map_or_else(today, iso_date);
    Metadata {
        title: if title == "Untitled" {
            String::new()
        } else {
            title.to_string()
        },
        created,
        ..Metadata::default()
    }
}

/// The mode the system gives a new file in `directory`, found by making one. If
/// that fails the answer is private.
pub fn default_mode(directory: &Path) -> u32 {
    let probe = directory.join(format!(".kame-mode-{}", uuid::Uuid::new_v4()));
    if std::fs::File::create_new(&probe).is_err() {
        return PRIVATE_MODE;
    }
    let mode = known_mode(&probe).ok().flatten().unwrap_or(PRIVATE_MODE);
    let _ = std::fs::remove_file(&probe);
    mode
}

fn known_mode(path: &Path) -> Result<Option<u32>, ()> {
    mode_from(std::fs::metadata(path))
}

/// A file's mode. A file that is not there has none; `Err` means it could not be
/// told, which callers treat as private.
fn mode_from(metadata: std::io::Result<std::fs::Metadata>) -> Result<Option<u32>, ()> {
    use std::os::unix::fs::PermissionsExt;
    match metadata {
        Ok(file) => Ok(Some(file.permissions().mode() & 0o777)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

/// The mode a saved file ends up with: the narrower of the file being replaced and
/// the document being saved, so a save never widens who can read either one's
/// content. With neither, the system's `default`.
pub fn saved_mode(target: Option<u32>, source: Option<u32>, default: u32) -> u32 {
    match (target, source) {
        (Some(target), Some(source)) => target & source,
        (Some(mode), None) | (None, Some(mode)) => mode,
        (None, None) => default,
    }
}

/// Where an export of `source` lands: `<stem>.<extension>` in the same
/// directory as the `.kame` file it was opened from.
///
/// A bare file name (no directory part) resolves in the current working
/// directory, which is also where a source with no usable stem falls back to —
/// an unnamed document exports as `document.md` rather than `..md`.
pub fn export_path(source: &Path, extension: &str) -> PathBuf {
    let name = match source.file_stem().filter(|stem| !stem.is_empty()) {
        Some(stem) => format!("{}.{extension}", stem.to_string_lossy()),
        None => format!("document.{extension}"),
    };
    match source.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        Some(dir) => dir.join(name),
        None => PathBuf::from(name),
    }
}

/// Whether `path` names a Markdown file to import rather than a `.kame` document.
pub fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        })
}

/// How many of `document`'s images point at a local file that is not there. A
/// remote image is not counted, since whether it exists is unknowable here.
/// The whole path of an image stored relative to the document at `document`. `None` for one
/// that isn't, which is to say a whole path already, a web address, or a document with no file.
fn whole_image_path(stored: &str, document: Option<&Path>) -> Option<PathBuf> {
    if Path::new(stored).is_absolute() || stored.contains("://") {
        return None;
    }
    document?;
    let file = image_file(stored, document);
    Some(file.canonicalize().unwrap_or(file))
}

pub fn missing_images(document: &Document) -> usize {
    document
        .blocks
        .iter()
        .filter(|block| match &block.kind {
            BlockKind::Image { path, .. } => !path.contains("://") && !Path::new(path).exists(),
            _ => false,
        })
        .count()
}

/// Maps a failure to open a document into an error a user can act on: it
/// blames the file (corrupt, foreign, or from a newer build) instead of
/// leaking a serde/sqlite detail that reads as "kame is broken".
pub fn open_error(path: &Path, err: DbError) -> std::io::Error {
    let reason = match err {
        DbError::Json(_) => {
            "the document is corrupt or was written by a different version of kame".to_string()
        }
        DbError::SchemaTooNew { found, supported } => format!(
            "the document was written by a newer version of kame \
             (schema {found}; this build supports {supported})"
        ),
        DbError::Sqlite(err) => {
            let detail = err.to_string();
            let lower = detail.to_ascii_lowercase();
            if lower.contains("locked") || lower.contains("busy") {
                format!("the document is already open in another kame instance ({detail})")
            } else if lower.contains("unable to open") {
                // Usually a wrong path or a missing directory, not corruption.
                format!("the file could not be opened — is the path correct? ({detail})")
            } else {
                format!("the file is not a valid kame document ({detail})")
            }
        }
        other => other.to_string(),
    };
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("cannot open {}: {reason}", path.display()),
    )
}

/// Today, as a metadata date.
pub fn today() -> String {
    iso_date(std::time::SystemTime::now())
}

/// What to say about dates in `metadata` that publishing won't be able to read. They are
/// the author's text and are saved regardless.
pub fn date_warning(metadata: &Metadata) -> Option<String> {
    [&metadata.created, &metadata.published]
        .into_iter()
        .find(|date| !date.trim().is_empty() && crate::model::Date::parse(date).is_none())
        .map(|date| format!("\"{date}\" isn't a date publishing can read: use YYYY-MM-DD"))
}
/// Whether undoing `old` and undoing `new` from `head` reach the same place, and redoing `new`
/// from there returns to `head`. Panics if either doesn't fit, as a log that has drifted won't.
fn histories_agree(head: &Document, old: &[Op], new: &[Op]) -> bool {
    use crate::edit::ops::{apply, apply_inverse};
    let rewind = |ops: &[Op]| {
        let mut doc = head.clone();
        for op in ops.iter().rev() {
            apply_inverse(&mut doc, op);
        }
        doc
    };
    let same = |a: &Document, b: &Document| {
        let shape = |doc: &Document| -> Vec<(uuid::Uuid, BlockKind, String)> {
            doc.blocks
                .iter()
                .map(|block| (block.id, block.kind.clone(), block.text.clone()))
                .collect()
        };
        let notes = |doc: &Document| {
            let mut ids: Vec<AnnotationId> = doc.annotations.iter().map(|note| note.id).collect();
            ids.sort();
            ids
        };
        shape(a) == shape(b) && notes(a) == notes(b)
    };
    let start = rewind(new);
    if !same(&start, &rewind(old)) {
        return false;
    }
    let mut forward = start;
    for op in new {
        apply(&mut forward, op);
    }
    same(&forward, head)
}

/// `when` as a `YYYY-MM-DD` day, in UTC.
pub fn iso_date(when: std::time::SystemTime) -> String {
    let secs = when
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    // Howard Hinnant's civil-from-days, on years that start in March so the leap
    // day falls at the end of one.
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
impl Session {
    fn insert_then_saved(&mut self, text: &str) -> bool {
        self.insert(text);
        !self.save_failed
    }
}

#[cfg(test)]
mod replace_text_tests {
    use super::*;

    fn session_with(text: &str) -> (Session, BlockId) {
        let mut session = Session::anonymous().unwrap();
        session.insert(text);
        let id = session.document().blocks[0].id;
        (session, id)
    }

    #[test]
    fn a_replacement_changes_the_text_and_is_one_undo() {
        let (mut session, id) = session_with("An indispensible tool.");
        let before = session.document().clone();
        assert!(session.replace_text(id, 3, 16, "indispensable"));
        assert_eq!(session.document().blocks[0].text, "An indispensable tool.");
        assert_eq!(session.caret.offset, 16);
        session.undo();
        assert_eq!(session.document(), &before);
    }

    #[test]
    fn a_removal_replaces_with_nothing() {
        let (mut session, id) = session_with("The the end.");
        assert!(session.replace_text(id, 0, 4, ""));
        assert_eq!(session.document().blocks[0].text, "the end.");
    }

    #[test]
    fn a_replacement_of_a_different_length_keeps_anchors_on_their_words() {
        let (mut session, id) = session_with("Bad wrod here.");
        session.set_caret(Cursor {
            block_id: id,
            offset: 9,
        });
        session.select_to(Cursor {
            block_id: id,
            offset: 13,
        });
        session.add_annotation(AnnotationKind::Note).unwrap();
        assert!(session.replace_text(id, 4, 8, "longerword"));
        let text = session.document().blocks[0].text.clone();
        let (start, end) = session.document().annotations[0]
            .anchor
            .byte_range()
            .unwrap();
        assert_eq!(&text[start..end], "here");
    }

    #[test]
    fn a_range_that_doesnt_fit_changes_nothing() {
        let (mut session, id) = session_with("caf\u{e9}");
        let before = session.document().clone();
        assert!(!session.replace_text(id, 0, 99, "x"));
        assert!(!session.replace_text(id, 4, 5, "x"), "inside a character");
        assert!(!session.replace_text(uuid::Uuid::new_v4(), 0, 1, "x"));
        assert_eq!(session.document(), &before);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path() -> PathBuf {
        let dir = crate::test_temp_dir().join(format!("kame-gui-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("doc.kame")
    }

    fn open(path: &Path) -> Session {
        Session::open(path).expect("open a session")
    }

    #[test]
    fn a_discarded_marginal_leaves_no_step_and_a_reopen_agrees() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("some words");
        let before = session.document().clone();
        let history = session.history();

        let id = session.add_annotation(AnnotationKind::Marginal).unwrap();
        assert!(session.discard_unwritten_marginal(id));

        assert_eq!(session.document(), &before);
        assert_eq!(session.history(), history);
        assert_eq!(session.history().1, 0, "nothing for a redo to bring back");
        assert!(!session.redo());
        assert_eq!(session.db.load_ops().unwrap().len(), history.0);
        assert_eq!(session.db.cursor().unwrap(), history.0);

        drop(session);
        let mut reopened = open(&path);
        assert_eq!(reopened.document(), &before);
        assert_eq!(reopened.history(), history);
        assert!(!reopened.redo());
        assert!(reopened.undo());
        assert!(reopened.document().blocks[0].text.is_empty());
    }

    #[test]
    fn editing_after_a_discarded_marginal_keeps_the_log_and_the_history_together() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("one");
        session.split();
        session.insert("two");
        let id = session.add_annotation(AnnotationKind::Marginal).unwrap();
        assert!(session.discard_unwritten_marginal(id));
        session.insert(" more");
        let (document, history) = (session.document().clone(), session.history());
        assert_eq!(session.db.load_ops().unwrap().len(), history.0);

        drop(session);
        let mut reopened = open(&path);
        assert_eq!(reopened.document(), &document);
        assert_eq!(reopened.history(), history);
        while reopened.undo() {}
        assert!(reopened.document().annotations.is_empty());
        assert_eq!(reopened.document().blocks.len(), 1);
        assert_eq!(reopened.document().blocks[0].text, "");
    }

    #[test]
    fn discarding_a_marginal_leaves_the_caret_and_selection_alone() {
        let mut session = Session::anonymous().unwrap();
        session.insert("some words here");
        let block_id = session.document().blocks[0].id;
        session.set_caret(Cursor {
            block_id,
            offset: 0,
        });
        session.select_to(Cursor {
            block_id,
            offset: 4,
        });
        let (caret, selection) = (session.caret, session.selection);
        assert!(selection.is_some());
        let id = session.add_annotation(AnnotationKind::Marginal).unwrap();
        assert!(session.discard_unwritten_marginal(id));
        assert_eq!(session.caret, caret);
        assert_eq!(session.selection, selection);
    }

    #[test]
    fn words_undone_in_a_marginal_can_still_come_back_after_it_is_closed() {
        let mut session = Session::anonymous().unwrap();
        session.insert("some words here");
        let block_id = session.document().blocks[0].id;
        session.set_caret(Cursor {
            block_id,
            offset: 0,
        });
        session.select_to(Cursor {
            block_id,
            offset: 4,
        });
        let (caret, selection) = (session.caret, session.selection);
        let id = session.add_annotation(AnnotationKind::Marginal).unwrap();
        assert!(session.set_annotation_body(id, "said something"));
        assert!(session.undo());
        session.caret = caret;
        session.selection = selection;

        assert!(session.discard_unwritten_marginal(id));

        assert!(session.document().annotations.is_empty());
        assert_eq!(session.caret, caret);
        assert_eq!(session.selection, selection);
        assert!(session.redo());
        assert!(session.redo());
        assert_eq!(session.document().annotations[0].body, "said something");
    }

    /// A session on `path` holding `a` and a marginal added after it, whose discard has just been
    /// refused by the file, once.
    fn session_after_a_failed_discard(path: &Path) -> (Session, AnnotationId) {
        let mut session = open(path);
        session.insert("a");
        let id = session.add_annotation(AnnotationKind::Marginal).unwrap();
        let outside = rusqlite::Connection::open(path).unwrap();
        outside
            .execute_batch(
                "CREATE TRIGGER refuse BEFORE DELETE ON ops \
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )
            .unwrap();
        session.discard_unwritten_marginal(id);
        outside.execute_batch("DROP TRIGGER refuse").unwrap();
        assert!(session.save_failed);
        (session, id)
    }

    /// That what the session holds is what its file holds: the same document, the same place in
    /// the history, and a log of just the steps it can undo and redo.
    fn assert_agrees_with_its_file(session: &Session) {
        let (undo, redo) = session.history();
        assert_eq!(session.db.load_ops().unwrap().len(), undo + redo);
        assert_eq!(session.db.cursor().unwrap(), undo);
        assert_eq!(&session.db.load_document().unwrap(), session.document());
    }

    #[test]
    fn a_failed_discard_leaves_the_marginal_in_memory_as_on_disk() {
        let path = temp_db_path();
        let (session, _) = session_after_a_failed_discard(&path);
        assert_agrees_with_its_file(&session);
        assert_eq!(session.document().annotations.len(), 1);
        assert_eq!(session.history(), (2, 0));
    }

    #[test]
    fn a_failed_discard_retried_and_typed_after_keeps_the_log_true() {
        let path = temp_db_path();
        let (mut session, _) = session_after_a_failed_discard(&path);
        assert!(session.retry_or_dismiss());
        assert!(!session.save_failed);
        session.insert("b");
        assert_agrees_with_its_file(&session);
        assert_eq!(session.document().blocks[0].text, "ab");
        let (document, history) = (session.document().clone(), session.history());

        drop(session);
        let mut reopened = open(&path);
        assert_agrees_with_its_file(&reopened);
        assert_eq!(reopened.document(), &document);
        assert_eq!(reopened.history(), history);
        assert!(!reopened.redo());
        assert_eq!(reopened.document().blocks[0].text, "ab");
    }

    #[test]
    fn a_failed_discard_retried_leaves_nothing_to_redo_after_a_reopen() {
        let path = temp_db_path();
        let (mut session, _) = session_after_a_failed_discard(&path);
        assert!(session.retry_or_dismiss());
        assert_agrees_with_its_file(&session);
        let document = session.document().clone();

        drop(session);
        let mut reopened = open(&path);
        assert_eq!(reopened.history().1, 0);
        assert!(!reopened.redo());
        assert_eq!(reopened.document(), &document);
    }

    #[test]
    fn a_failed_discard_never_retried_reopens_with_the_marginal() {
        let path = temp_db_path();
        let (session, _) = session_after_a_failed_discard(&path);
        let (document, history) = (session.document().clone(), session.history());

        drop(session);
        let reopened = open(&path);
        assert_agrees_with_its_file(&reopened);
        assert_eq!(reopened.document(), &document);
        assert_eq!(reopened.history(), history);
        assert_eq!(reopened.document().annotations.len(), 1);
    }

    #[test]
    fn a_failed_discard_can_be_tried_again_once_the_write_is_retried() {
        let path = temp_db_path();
        let (mut session, id) = session_after_a_failed_discard(&path);
        assert!(session.retry_or_dismiss());

        assert!(session.discard_unwritten_marginal(id));

        assert!(session.document().annotations.is_empty());
        assert_eq!(session.history(), (1, 0));
        assert_agrees_with_its_file(&session);
    }

    #[test]
    fn saying_what_a_note_already_says_is_not_an_edit() {
        for kind in [
            AnnotationKind::Note,
            AnnotationKind::Footnote,
            AnnotationKind::Marginal,
        ] {
            let mut session = Session::anonymous().unwrap();
            session.insert("word");
            let id = session.add_annotation(kind).unwrap();
            let body = session.document().annotations[0].body.clone();
            let history = session.history();
            assert!(session.set_annotation_body(id, &body));
            assert_eq!(session.history(), history);
            assert_eq!(session.db.load_ops().unwrap().len(), history.0);
        }
    }

    fn linked_document() -> Session {
        let mut session = Session::anonymous().unwrap();
        let mut document = session.document().clone();
        let block = &mut document.blocks[0];
        block.text = "see here and there".to_string();
        block.spans = vec![
            Span {
                start: 4,
                end: 8,
                style: SpanStyle::DocumentLink {
                    target: "old.kame".to_string(),
                },
            },
            Span {
                start: 9,
                end: 12,
                style: SpanStyle::Link {
                    url: "old.kame".to_string(),
                },
            },
        ];
        document.blocks.push(crate::model::Block {
            id: BlockId::new_v4(),
            kind: BlockKind::Image {
                path: "plate.png".to_string(),
                link: Some("old.kame".to_string()),
                align: ImageAlign::Left,
            },
            text: String::new(),
            spans: vec![],
        });
        assert!(session.replace_document(document));
        session
    }

    #[test]
    fn rewriting_links_changes_both_kinds_and_an_images_link_as_one_undo_step() {
        let mut session = linked_document();
        let before = session.document().clone();
        let history = session.history();
        let changed =
            session.rewrite_links(|target| (target == "old.kame").then(|| "new.kame".to_string()));
        assert_eq!(changed, Ok(3));
        assert_eq!(session.history().0, history.0 + 1);
        let after = session.document();
        assert_eq!(after.blocks[0].text, before.blocks[0].text);
        assert_eq!(after.annotations, before.annotations);
        assert_eq!(
            after.blocks[0].spans[0].style,
            SpanStyle::DocumentLink {
                target: "new.kame".to_string()
            }
        );
        assert_eq!(
            after.blocks[0].spans[1].style,
            SpanStyle::Link {
                url: "new.kame".to_string()
            }
        );
        assert!(matches!(
            &after.blocks[1].kind,
            BlockKind::Image { link: Some(link), .. } if link == "new.kame"
        ));
        assert!(session.undo());
        assert_eq!(session.document(), &before);
    }

    #[test]
    fn rewriting_no_links_writes_nothing() {
        let mut session = linked_document();
        let history = session.history();
        assert_eq!(session.rewrite_links(|_| None), Ok(0));
        assert_eq!(
            session.rewrite_links(|target| Some(target.to_string())),
            Ok(0)
        );
        assert_eq!(session.history(), history);
    }

    #[test]
    fn rewriting_links_refuses_a_document_from_a_newer_version() {
        let mut session = linked_document();
        session.mark_newer();
        assert!(session.rewrite_links(|_| Some("x".to_string())).is_err());
    }

    #[test]
    fn a_rename_changes_a_title_still_matching_the_old_name() {
        let mut session = Session::anonymous().unwrap();
        let mut document = session.document().clone();
        document.title = "foo".to_string();
        session.replace_document(document);
        session.set_metadata(Metadata {
            title: "foo".to_string(),
            ..session.metadata.clone()
        });
        assert!(session.follow_rename("foo", "bar"));
        assert_eq!(session.metadata.title, "bar");
        assert_eq!(session.document().title, "bar");
    }

    #[test]
    fn a_rename_leaves_a_title_the_author_set() {
        let mut session = Session::anonymous().unwrap();
        session.set_metadata(Metadata {
            title: "On Matter".to_string(),
            ..session.metadata.clone()
        });
        assert!(!session.follow_rename("foo", "bar"));
        assert_eq!(session.metadata.title, "On Matter");
        let mut document = session.document().clone();
        document.title = "foo".to_string();
        session.replace_document(document);
        assert!(session.follow_rename("foo", "bar"));
        assert_eq!(session.metadata.title, "On Matter");
        assert_eq!(session.document().title, "bar");
    }

    #[test]
    fn saving_as_clears_the_sync_state() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.set_sync_state("x");
        session.set_sync_place("here.kame");
        assert_eq!(session.sync_state().as_deref(), Some("x"));
        assert_eq!(session.sync_place().as_deref(), Some("here.kame"));
        let other = path.with_file_name("other.kame");
        session.save_as(&other).unwrap();
        assert_eq!(session.sync_state(), None);
        assert_eq!(session.sync_place(), None);
    }

    fn first_block_id(session: &Session) -> crate::model::BlockId {
        session.document().blocks[0].id
    }

    fn note_on(text: &str, from: usize, to: usize, kind: AnnotationKind) -> (usize, usize) {
        let mut session = Session::anonymous().unwrap();
        session.insert(text);
        let block_id = session.document().blocks[0].id;
        session.set_caret(Cursor {
            block_id,
            offset: from,
        });
        session.select_to(Cursor {
            block_id,
            offset: to,
        });
        session
            .annotation_anchor(kind)
            .unwrap()
            .byte_range()
            .unwrap()
    }

    #[test]
    fn a_footnote_on_part_of_a_word_takes_the_whole_word() {
        let text = "This has a margin note, a footnote here.";
        let at = |word: &str| text.find(word).unwrap();
        let (start, end) = note_on(text, at("te,"), at("tnote"), AnnotationKind::Footnote);
        assert_eq!(&text[start..end], "note, a footnote");
        let (start, end) = note_on(text, at("rgin"), at("rgin") + 2, AnnotationKind::Footnote);
        assert_eq!(&text[start..end], "margin");
    }

    #[test]
    fn a_highlight_keeps_exactly_what_was_selected() {
        let text = "This has a margin note, a footnote here.";
        let from = text.find("te,").unwrap();
        let to = text.find("tnote").unwrap();
        let (start, end) = note_on(text, from, to, AnnotationKind::Note);
        assert_eq!(&text[start..end], "te, a foo");
    }

    #[test]
    fn a_footnote_on_whole_words_stays_as_it_was() {
        let text = "One two, three.";
        let (start, end) = note_on(text, 4, 7, AnnotationKind::Footnote);
        assert_eq!(&text[start..end], "two");
        let (start, end) = note_on(text, 3, 8, AnnotationKind::Footnote);
        assert_eq!(
            &text[start..end],
            " two,",
            "punctuation and space stay as chosen"
        );
    }

    #[test]
    fn widening_a_footnote_keeps_to_characters() {
        let text = "Le café—noir, déjà.";
        let inside = text.find("fé").unwrap();
        let (start, end) = note_on(text, inside, inside + "f".len(), AnnotationKind::Footnote);
        assert_eq!(&text[start..end], "café");
    }

    #[test]
    fn the_first_save_of_an_import_suggests_its_source_name() {
        let dir = crate::test_temp_dir().join(format!("kame-name-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.md");
        std::fs::write(&path, "Title: Something Else\n\nWords.\n").unwrap();
        let (session, _) = Session::open_any(&path).unwrap();
        assert_eq!(session.suggested_file_name(), "notes.kame");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_new_document_suggests_untitled() {
        let session = Session::anonymous().unwrap();
        assert_eq!(session.suggested_file_name(), "Untitled.kame");
    }

    /// A document keeps the title it was created with, so one started as Untitled
    /// and saved under a name would otherwise be Untitled for good.
    #[test]
    fn a_saved_untitled_document_is_named_by_its_file() {
        let path = temp_db_path().with_file_name("Document.kame");
        let mut session = Session::anonymous().unwrap();
        session.insert("Words");
        assert_eq!(session.display_title(), "Untitled");
        session.save_as(&path).unwrap();
        assert_eq!(session.display_title(), "Document");
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(
            reopened.document().title,
            "Untitled",
            "the stored title is untouched"
        );
        assert_eq!(reopened.display_title(), "Document");
    }

    #[test]
    fn a_real_title_outranks_the_file_name() {
        let dir = crate::test_temp_dir().join(format!("kame-title-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("notes.md");
        std::fs::write(&source, "Title: A Real Title\n\nWords.\n").unwrap();
        let (mut session, _) = Session::open_any(&source).unwrap();
        assert_eq!(session.display_title(), "A Real Title");
        session.save_as(&dir.join("notes.kame")).unwrap();
        assert_eq!(session.display_title(), "A Real Title");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dates_are_written_as_iso_days() {
        use std::time::{Duration, UNIX_EPOCH};
        let day = |secs: u64| iso_date(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(951_782_400), "2000-02-29");
        assert_eq!(day(1_793_577_600), "2026-11-02");
        assert_eq!(day(1_735_603_200), "2024-12-31");
        assert_eq!(day(4_107_542_400), "2100-03-01");
    }

    #[test]
    fn a_new_document_is_created_today_and_goes_by_untitled() {
        let session = Session::anonymous().unwrap();
        assert_eq!(
            session.metadata.created,
            iso_date(std::time::SystemTime::now())
        );
        assert_eq!(session.metadata.title, "");
        assert_eq!(session.display_title(), "Untitled");
    }

    #[test]
    fn metadata_persists_across_a_reopen_outside_the_undo_log() {
        let path = temp_db_path();
        let mut session = open(&path);
        let before = session.history();
        session.set_metadata(Metadata {
            title: "Stored".to_string(),
            author: "K".to_string(),
            ..session.metadata.clone()
        });
        assert_eq!(session.history(), before, "not an undo step");
        assert_eq!(session.display_title(), "Stored");
        drop(session);
        let reopened = open(&path);
        assert_eq!(reopened.metadata.title, "Stored");
        assert_eq!(reopened.metadata.author, "K");
    }

    #[test]
    fn save_as_carries_metadata_to_the_file() {
        let path = temp_db_path();
        let mut session = Session::anonymous().unwrap();
        session.insert("Words");
        session.set_metadata(Metadata {
            title: "Carried".to_string(),
            ..session.metadata.clone()
        });
        session.save_as(&path).unwrap();
        drop(session);
        assert_eq!(open(&path).metadata.title, "Carried");
    }

    #[test]
    fn restoring_a_snapshot_leaves_metadata_alone() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("Words");
        let id = session.snapshot(None).expect("a snapshot");
        session.set_metadata(Metadata {
            title: "After".to_string(),
            ..session.metadata.clone()
        });
        assert!(session.restore(id));
        assert_eq!(session.metadata.title, "After");
    }

    /// A file from before the metadata table takes its title from the document,
    /// unless that was never named, and its date from the file.
    #[test]
    fn an_older_file_gets_reasonable_defaults() {
        let path = temp_db_path().with_file_name("Essay.kame");
        let mut session = Session::anonymous().unwrap();
        session.insert("Words");
        session.save_as(&path).unwrap();
        drop(session);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE metadata").ok();
        drop(conn);
        let reopened = open(&path);
        assert_eq!(reopened.metadata.title, "", "Untitled was never a name");
        assert_eq!(reopened.display_title(), "Essay");
        assert!(!reopened.metadata.created.is_empty());
        assert_eq!(reopened.metadata.author, "");
    }

    #[test]
    fn an_import_keeps_its_front_matter() {
        let dir = crate::test_temp_dir().join(format!("kame-meta-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("notes.md");
        std::fs::write(&source, "Title: T\nAuthor: K\nTags: a, b\n\nWords.\n").unwrap();
        let (session, _) = Session::open_any(&source).unwrap();
        assert_eq!(session.metadata.title, "T");
        assert_eq!(session.metadata.author, "K");
        assert_eq!(session.metadata.tags, vec!["a", "b"]);
        assert!(!session.metadata.created.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn exports_publish_the_metadata() {
        let path = temp_db_path().with_file_name("Pub.kame");
        let mut session = open(&path);
        session.insert("Words");
        session.set_metadata(Metadata {
            author: "K".to_string(),
            ..session.metadata.clone()
        });
        let markdown_path = session.export_markdown().expect("markdown written");
        let markdown = std::fs::read_to_string(markdown_path).unwrap();
        assert!(markdown.contains("Author: K\n"), "{markdown}");
        let html_path = session.export_html().expect("html written");
        let html = std::fs::read_to_string(html_path).unwrap();
        assert!(
            html.contains(r#"<meta name="author" content="K">"#),
            "{html}"
        );
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = crate::test_temp_dir().join(format!("kame-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_export_suggests_a_name_beside_the_document() {
        let dir = scratch_dir("suggest");
        let session = open(&dir.join("essay.kame"));
        assert_eq!(session.export_suggestion("md"), dir.join("essay.md"));
        assert_eq!(session.export_suggestion("html"), dir.join("essay.html"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unsaved_import_exports_beside_its_source() {
        let dir = scratch_dir("suggest");
        let source = dir.join("notes.md");
        std::fs::write(&source, "Words.\n").unwrap();
        let (session, _) = Session::open_any(&source).unwrap();
        assert_eq!(session.export_suggestion("html"), dir.join("notes.html"));
        assert_eq!(session.dialog_directory(), Some(dir.clone()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_new_document_suggests_a_bare_name_and_no_directory() {
        let session = Session::anonymous().unwrap();
        assert_eq!(
            session.export_suggestion("md"),
            PathBuf::from("Untitled.md")
        );
        assert_eq!(session.dialog_directory(), None);
    }

    #[test]
    fn an_export_writes_where_it_is_told() {
        let dir = scratch_dir("export");
        let mut session = open(&dir.join("essay.kame"));
        session.insert("Words");
        let target = dir.join("elsewhere.md");
        assert_eq!(
            session.export_markdown_to(&target, &References::new()),
            Some(target.clone())
        );
        assert!(std::fs::read_to_string(&target).unwrap().contains("Words"));
        assert!(!dir.join("essay.md").exists());
        let html = dir.join("page.html");
        assert_eq!(
            session.export_html_to(&html, &References::new()),
            Some(html.clone())
        );
        assert!(std::fs::read_to_string(&html).unwrap().contains("Words"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A notebook-shaped folder with one plate in it, for the tests of image paths.
    fn plate_folder(name: &str) -> (PathBuf, PathBuf) {
        let root = scratch_dir(name).canonicalize().unwrap();
        std::fs::create_dir_all(root.join("static")).unwrap();
        std::fs::create_dir_all(root.join("maritain")).unwrap();
        let plate = root.join("static/plate.png");
        std::fs::write(&plate, "a plate").unwrap();
        (root, plate)
    }

    fn image_paths(session: &Session) -> Vec<String> {
        session
            .document()
            .blocks
            .iter()
            .filter_map(|block| match &block.kind {
                BlockKind::Image { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

    fn image_align(session: &Session) -> Vec<ImageAlign> {
        session
            .document()
            .blocks
            .iter()
            .filter_map(|block| match &block.kind {
                BlockKind::Image { align, .. } => Some(*align),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn aligning_an_image_keeps_it_and_undoes_as_one_step() {
        let (root, _plate) = plate_folder("align");
        let path = root.join("essay.kame");
        let mut session = open(&path);
        session.insert_image("static/plate.png".to_string());
        let image = session.valid_caret().block_id;
        assert!(session.set_image_align(image, ImageAlign::Center));
        assert_eq!(image_align(&session), [ImageAlign::Center]);
        assert_eq!(image_paths(&session), ["static/plate.png"]);
        assert!(session.undo());
        assert_eq!(image_align(&session), [ImageAlign::Left]);
        assert!(session.set_image_align(image, ImageAlign::Right));
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(image_align(&reopened), [ImageAlign::Right]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Every link target in the document, in reading order, an image's link after its block's spans.
    fn link_targets(session: &Session) -> Vec<String> {
        let mut targets = Vec::new();
        for block in &session.document().blocks {
            for span in &block.spans {
                match &span.style {
                    SpanStyle::Link { url } => targets.push(url.clone()),
                    SpanStyle::DocumentLink { target } => targets.push(target.clone()),
                    _ => {}
                }
            }
            if let BlockKind::Image {
                link: Some(link), ..
            } = &block.kind
            {
                targets.push(link.clone());
            }
        }
        targets
    }

    /// A document at `root/one/a.kame` holding each of `targets`, as a `DocumentLink` if it says so
    /// and otherwise as a `Link`.
    fn document_with_links(root: &Path, targets: &[(&str, bool)]) -> Session {
        std::fs::create_dir_all(root.join("one")).unwrap();
        let mut session = open(&root.join("one/a.kame"));
        let mut document = session.document().clone();
        let block = &mut document.blocks[0];
        block.text = "x".repeat(targets.len());
        block.spans = targets
            .iter()
            .enumerate()
            .map(|(at, (target, is_document))| Span {
                start: at,
                end: at + 1,
                style: if *is_document {
                    SpanStyle::DocumentLink {
                        target: target.to_string(),
                    }
                } else {
                    SpanStyle::Link {
                        url: target.to_string(),
                    }
                },
            })
            .collect();
        session.replace_document(document);
        session
    }

    /// A document at `root/one/a.kame` holding one of each kind of link: the two that depend on
    /// where it is first, as `Link` and `DocumentLink` and an image's link, and then the rest.
    fn document_with_each_link(root: &Path) -> Session {
        let targets: [(&str, bool); 8] = [
            ("../two/d.kame", true),
            ("../two/d.kame#Part", true),
            ("../notes.md", false),
            ("#Heading", true),
            ("#Heading", false),
            ("https://example.com/a#b", false),
            ("mailto:a@example.com", false),
            ("www.example.com/x", false),
        ];
        std::fs::create_dir_all(root.join("two")).unwrap();
        std::fs::write(root.join("two/d.kame"), "").unwrap();
        std::fs::write(root.join("notes.md"), "").unwrap();
        let mut session = document_with_links(root, &targets);
        session.insert_image("https://example.com/plate.png".to_string());
        let image = session.valid_caret().block_id;
        session.set_block_kind(
            image,
            BlockKind::Image {
                path: "https://example.com/plate.png".to_string(),
                link: Some("../two/d.kame".to_string()),
                align: ImageAlign::default(),
            },
        );
        session
    }

    #[test]
    fn saving_as_says_a_relative_link_from_the_new_folder() {
        let root = scratch_dir("rehomelinks").canonicalize().unwrap();
        let from = root.join("one/a.kame");
        let mut session = document_with_each_link(&root);
        std::fs::create_dir_all(root.join("three/deep")).unwrap();
        let to = root.join("three/deep/a.kame");
        session.save_as(&to).unwrap();
        session.rehome_links(Some(&from));
        let expected = [
            "../../two/d.kame",
            "../../two/d.kame#Part",
            "../../notes.md",
            "#Heading",
            "#Heading",
            "https://example.com/a#b",
            "mailto:a@example.com",
            "www.example.com/x",
            "../../two/d.kame",
        ];
        assert_eq!(link_targets(&session), expected);
        // It reached the disk, and is one step that goes back to what it was.
        drop(session);
        let mut reopened = Session::open(&to).unwrap();
        assert_eq!(link_targets(&reopened), expected);
        assert!(reopened.undo());
        assert_eq!(link_targets(&reopened)[0], "../two/d.kame");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_as_within_a_folder_or_with_no_file_before_changes_no_link() {
        let root = scratch_dir("rehomelinkssame").canonicalize().unwrap();
        let from = root.join("one/a.kame");
        let mut session = document_with_each_link(&root);
        let before = link_targets(&session);
        let history = session.history();
        session.save_as(&root.join("one/b.kame")).unwrap();
        session.rehome_links(Some(&from));
        assert_eq!(link_targets(&session), before);
        assert_eq!(session.history(), history);
        session.rehome_links(None);
        assert_eq!(link_targets(&session), before);
        assert_eq!(session.history(), history);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The links a document has to itself, one of each way of writing that, in both kinds of link.
    const LINKS_TO_ITSELF: [(&str, bool); 8] = [
        ("a.kame", true),
        ("a.kame#Part", true),
        ("a", true),
        ("./a.kame#Part", true),
        ("../one/a.kame", true),
        ("a.kame", false),
        ("a.kame#Part", false),
        ("#Part", true),
    ];

    #[test]
    fn saving_as_elsewhere_keeps_a_link_to_itself_leading_to_itself() {
        let root = scratch_dir("rehomeself").canonicalize().unwrap();
        let from = root.join("one/a.kame");
        let mut session = document_with_links(&root, &LINKS_TO_ITSELF);
        std::fs::create_dir_all(root.join("three/deep")).unwrap();
        session.save_as(&root.join("three/deep/a.kame")).unwrap();
        session.rehome_links(Some(&from));
        assert_eq!(
            link_targets(&session),
            [
                "a.kame",
                "a.kame#Part",
                "a.kame",
                "a.kame#Part",
                "a.kame",
                "a.kame",
                "a.kame#Part",
                "#Part",
            ]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_as_another_name_keeps_a_link_to_itself_leading_to_itself() {
        let root = scratch_dir("rehomeselfname").canonicalize().unwrap();
        let from = root.join("one/a.kame");
        let mut session = document_with_links(&root, &LINKS_TO_ITSELF);
        session.save_as(&root.join("one/b.kame")).unwrap();
        session.rehome_links(Some(&from));
        assert_eq!(
            link_targets(&session),
            [
                "b.kame",
                "b.kame#Part",
                "b.kame",
                "b.kame#Part",
                "b.kame",
                "b.kame",
                "b.kame#Part",
                "#Part",
            ]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_as_keeps_a_link_to_a_folders_document_when_it_is_that_document() {
        let root = scratch_dir("rehomeselffolder").canonicalize().unwrap();
        std::fs::create_dir_all(root.join("one")).unwrap();
        let from = root.join("one/Notebook.kame");
        let mut session = open(&from);
        let mut document = session.document().clone();
        document.blocks[0].text = "x".to_string();
        document.blocks[0].spans = vec![Span {
            start: 0,
            end: 1,
            style: SpanStyle::DocumentLink {
                target: "./#Part".to_string(),
            },
        }];
        session.replace_document(document);
        session.save_as(&root.join("one/b.kame")).unwrap();
        session.rehome_links(Some(&from));
        assert_eq!(link_targets(&session), ["b.kame#Part"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_as_onto_its_own_file_leaves_a_link_to_itself_as_written() {
        let root = scratch_dir("rehomeselfsame").canonicalize().unwrap();
        let from = root.join("one/a.kame");
        let mut session = document_with_links(&root, &LINKS_TO_ITSELF);
        let (before, history) = (link_targets(&session), session.history());
        session.save_as(&from).unwrap();
        session.rehome_links(Some(&from));
        assert_eq!(link_targets(&session), before);
        assert_eq!(session.history(), history);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_as_leaves_a_link_alone_unless_it_names_something_there_was() {
        let root = scratch_dir("rehomenothing").canonicalize().unwrap();
        std::fs::create_dir_all(root.join("two")).unwrap();
        std::fs::write(root.join("two/there.pdf"), "").unwrap();
        std::fs::write(root.join("two/d.kame"), "").unwrap();
        let from = root.join("one/a.kame");
        let mut session = document_with_links(
            &root,
            &[
                ("example.com/x", false),
                ("me@example.com", false),
                ("localhost/x", false),
                ("?q=1", false),
                ("../two/gone.pdf", false),
                ("../two/there.pdf", false),
                ("../two/d", false),
                ("../two/", false),
                ("../two/d.kame#Part", false),
                ("../two/gone.kame", true),
            ],
        );
        session.insert_image("https://example.com/plate.png".to_string());
        let image = session.valid_caret().block_id;
        session.set_block_kind(
            image,
            BlockKind::Image {
                path: "https://example.com/plate.png".to_string(),
                link: Some("example.com/y".to_string()),
                align: ImageAlign::default(),
            },
        );
        std::fs::create_dir_all(root.join("three/deep")).unwrap();
        session.save_as(&root.join("three/deep/a.kame")).unwrap();
        session.rehome_links(Some(&from));
        assert_eq!(
            link_targets(&session),
            [
                "example.com/x",
                "me@example.com",
                "localhost/x",
                "?q=1",
                "../two/gone.pdf",
                "../../two/there.pdf",
                "../../two/d",
                "../../two/",
                "../../two/d.kame#Part",
                "../../two/gone.kame",
                "example.com/y",
            ]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_as_keeps_the_selection_when_links_change() {
        let root = scratch_dir("rehomelinkssel").canonicalize().unwrap();
        let from = root.join("one/a.kame");
        let mut session = document_with_each_link(&root);
        let block_id = session.document().blocks[0].id;
        session.set_caret(Cursor {
            block_id,
            offset: 1,
        });
        session.select_to(Cursor {
            block_id,
            offset: 3,
        });
        let selection = session.selection;
        assert!(selection.is_some());
        std::fs::create_dir_all(root.join("three")).unwrap();
        session.save_as(&root.join("three/a.kame")).unwrap();
        session.rehome_links(Some(&from));
        assert_eq!(session.selection, selection);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn aligning_to_where_it_is_or_anything_else_changes_nothing() {
        let (root, _plate) = plate_folder("alignnone");
        let mut session = open(&root.join("essay.kame"));
        let paragraph = session.valid_caret().block_id;
        assert!(!session.set_image_align(paragraph, ImageAlign::Center));
        session.insert_image("static/plate.png".to_string());
        let image = session.valid_caret().block_id;
        assert!(!session.set_image_align(image, ImageAlign::Left));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_as_keeps_an_images_alignment() {
        let (root, _plate) = plate_folder("rehomealign");
        let first = root.join("essay.kame");
        let second = root.join("maritain/essay.kame");
        let mut session = open(&first);
        session.insert_image("static/plate.png".to_string());
        let image = session.valid_caret().block_id;
        session.set_image_align(image, ImageAlign::Center);
        session.save_as(&second).unwrap();
        session.rehome_images(Some(&first), Some(&root));
        assert_eq!(image_paths(&session), ["../static/plate.png"]);
        assert_eq!(image_align(&session), [ImageAlign::Center]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_as_says_an_images_path_from_the_new_folder() {
        let (root, _plate) = plate_folder("rehome");
        let first = root.join("essay.kame");
        let second = root.join("maritain/essay.kame");
        let mut session = open(&first);
        session.insert_image("static/plate.png".to_string());
        session.save_as(&second).unwrap();
        session.rehome_images(Some(&first), Some(&root));
        assert_eq!(image_paths(&session), ["../static/plate.png"]);
        drop(session);
        let reopened = Session::open(&second).unwrap();
        assert_eq!(image_paths(&reopened), ["../static/plate.png"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_an_unsaved_document_into_a_notebook_shortens_its_images() {
        let (root, plate) = plate_folder("unsaved");
        let target = root.join("maritain/essay.kame");
        let mut session = Session::anonymous().unwrap();
        session.insert_image(plate.display().to_string());
        session.save_as(&target).unwrap();
        session.rehome_images(None, Some(&root));
        assert_eq!(image_paths(&session), ["../static/plate.png"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_as_the_same_file_shortens_an_old_documents_images() {
        let (root, plate) = plate_folder("sameplace");
        let document = root.join("maritain/essay.kame");
        let mut session = open(&document);
        session.insert_image(plate.display().to_string());
        session.save_as(&document).unwrap();
        session.rehome_images(Some(&document), Some(&root));
        assert_eq!(image_paths(&session), ["../static/plate.png"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_out_of_a_notebook_gives_an_image_its_whole_path() {
        let (root, plate) = plate_folder("leaving");
        let first = root.join("essay.kame");
        let away = scratch_dir("leaving-away").canonicalize().unwrap();
        let mut session = open(&first);
        session.insert_image("static/plate.png".to_string());
        session.save_as(&away.join("essay.kame")).unwrap();
        session.rehome_images(Some(&first), None);
        assert_eq!(image_paths(&session), [plate.display().to_string()]);
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&away).unwrap();
    }

    #[test]
    fn saving_as_leaves_alone_an_image_it_cannot_find() {
        let (root, _plate) = plate_folder("unfound");
        let first = root.join("essay.kame");
        let mut session = open(&first);
        for stored in ["gone.png", "/files/a.png", "https://e.com/a.png"] {
            session.insert_image(stored.to_string());
        }
        let before = image_paths(&session);
        session.save_as(&root.join("maritain/essay.kame")).unwrap();
        session.rehome_images(Some(&first), Some(&root));
        assert_eq!(image_paths(&session), before);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_export_elsewhere_can_still_find_the_image() {
        let (root, plate) = plate_folder("exported");
        let document = root.join("maritain/essay.kame");
        let mut session = open(&document);
        session.insert_image("../static/plate.png".to_string());
        let target = root.with_extension("md");
        session
            .export_markdown_to(&target, &References::new())
            .unwrap();
        let written = std::fs::read_to_string(&target).unwrap();
        assert!(written.contains(&plate.display().to_string()), "{written}");
        assert_eq!(image_paths(&session), ["../static/plate.png"]);
        std::fs::remove_file(&target).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The native dialog has already confirmed the overwrite, so nothing of the old
    /// file may survive into the new one.
    #[test]
    fn saving_over_a_file_replaces_it_entirely() {
        let dir = scratch_dir("over");
        let target = dir.join("target.kame");
        let mut old = open(&target);
        old.insert("Old words");
        old.snapshot(None).expect("a snapshot");
        drop(old);

        let mut session = Session::anonymous().unwrap();
        session.insert("New");
        session.save_as(&target).unwrap();
        assert_eq!(session.history(), (1, 0));
        drop(session);

        let reopened = open(&target);
        assert_eq!(reopened.document().blocks[0].text, "New");
        assert_eq!(reopened.history(), (1, 0), "the old history is gone");
        assert!(reopened.db.list_snapshots().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_as_from_a_file_keeps_history_and_snapshots_and_the_original() {
        let dir = scratch_dir("copy");
        let first = dir.join("first.kame");
        let second = dir.join("second.kame");
        let mut session = open(&first);
        session.insert("Words");
        session.snapshot(None).expect("a snapshot");
        session.save_as(&second).unwrap();
        assert_eq!(session.path(), Some(second.as_path()));
        session.insert(" more");
        drop(session);

        let copy = open(&second);
        assert_eq!(copy.document().blocks[0].text, "Words more");
        assert_eq!(copy.history(), (2, 0));
        assert_eq!(copy.db.list_snapshots().unwrap().len(), 1);
        drop(copy);
        let original = open(&first);
        assert_eq!(
            original.document().blocks[0].text,
            "Words",
            "edits went to the copy"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_onto_the_same_file_loses_nothing() {
        let dir = scratch_dir("same");
        let path = dir.join("doc.kame");
        let mut session = open(&path);
        session.insert("Words");
        session.save_as(&path).unwrap();
        session.insert(" more");
        drop(session);
        let reopened = open(&path);
        assert_eq!(reopened.document().blocks[0].text, "Words more");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_save_leaves_the_session_and_the_target_as_they_were() {
        let dir = scratch_dir("fail");
        let target = dir.join("no-such-directory").join("doc.kame");
        let mut session = Session::anonymous().unwrap();
        session.insert("Words");
        assert!(session.save_as(&target).is_err());
        assert!(session.is_anonymous());
        session.insert(" more");
        assert_eq!(session.document().blocks[0].text, "Words more");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn saving_over_a_private_file_keeps_it_private() {
        let dir = scratch_dir("mode");
        let target = dir.join("private.kame");
        drop(open(&target));
        set_mode(&target, 0o600);
        let mut session = Session::anonymous().unwrap();
        session.insert("Words");
        session.save_as(&target).unwrap();
        assert_eq!(mode_of(&target), 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn save_as_from_a_private_file_stays_private() {
        let dir = scratch_dir("mode");
        let source = dir.join("private.kame");
        let mut session = open(&source);
        session.insert("Words");
        set_mode(&source, 0o600);
        let copy = dir.join("copy.kame");
        session.save_as(&copy).unwrap();
        assert_eq!(mode_of(&copy), 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Its mode is gone with it, and may have been private.
    #[cfg(unix)]
    #[test]
    fn saving_after_the_document_file_vanished_stays_private() {
        let dir = scratch_dir("mode");
        let source = dir.join("gone.kame");
        let mut session = open(&source);
        session.insert("Words");
        std::fs::remove_file(&source).unwrap();
        let copy = dir.join("copy.kame");
        session.save_as(&copy).unwrap();
        assert_eq!(mode_of(&copy), PRIVATE_MODE);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn saving_an_import_of_a_private_file_stays_private() {
        let dir = scratch_dir("mode");
        let source = dir.join("private.md");
        std::fs::write(&source, "Words.\n").unwrap();
        set_mode(&source, 0o600);
        let (mut session, _) = Session::open_any(&source).unwrap();
        let target = dir.join("private.kame");
        session.save_as(&target).unwrap();
        assert_eq!(mode_of(&target), 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_saved_mode_never_widens_either_side() {
        assert_eq!(saved_mode(Some(0o640), Some(0o600), 0o644), 0o600);
        assert_eq!(saved_mode(Some(0o600), Some(0o644), 0o644), 0o600);
        assert_eq!(saved_mode(Some(0o644), Some(0o664), 0o644), 0o644);
        assert_eq!(saved_mode(None, Some(0o640), 0o644), 0o640);
        assert_eq!(saved_mode(Some(0o640), None, 0o644), 0o640);
        assert_eq!(saved_mode(None, None, 0o644), 0o644);
    }

    /// A file whose mode cannot be read is treated as private, not as default.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_mode_fails_closed() {
        assert_eq!(known_mode(Path::new("/no/such/file")), Ok(None));
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(mode_from(Err(denied)), Err(()));
    }

    #[cfg(unix)]
    #[test]
    fn a_new_document_gets_the_systems_default_mode() {
        let dir = scratch_dir("mode");
        let plain = dir.join("plain.txt");
        std::fs::write(&plain, "x").unwrap();
        let target = dir.join("new.kame");
        let mut session = Session::anonymous().unwrap();
        session.insert("Words");
        session.save_as(&target).unwrap();
        assert_eq!(mode_of(&target), mode_of(&plain));
        let left = std::fs::read_dir(&dir).unwrap().count();
        assert!(left <= 4, "stray files after a save: {left}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn saving_a_private_document_over_a_shared_file_stays_private() {
        let dir = scratch_dir("mode");
        let source = dir.join("private.kame");
        let target = dir.join("shared.kame");
        drop(open(&target));
        set_mode(&target, 0o640);
        let mut session = open(&source);
        session.insert("Words");
        set_mode(&source, 0o600);
        session.save_as(&target).unwrap();
        assert_eq!(mode_of(&target), 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_copy_leaves_no_partial_file_behind() {
        let dir = scratch_dir("partial");
        let target = dir.join("doc.kame");
        std::fs::create_dir(&target).unwrap();
        let mut session = Session::anonymous().unwrap();
        session.insert("Words");
        assert!(
            session.save_as(&target).is_err(),
            "a directory cannot be replaced"
        );
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("doc.kame")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn markdown_paths_are_recognised_by_extension() {
        for path in ["a.md", "a.markdown", "a.MD", "/x/y.Markdown"] {
            assert!(is_markdown_path(Path::new(path)), "{path}");
        }
        for path in ["a.kame", "md", "a.md.kame", "a"] {
            assert!(!is_markdown_path(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn opening_a_missing_markdown_file_fails_without_creating_it() {
        let path = crate::test_temp_dir().join(format!("kame-missing-{}.md", uuid::Uuid::new_v4()));
        assert!(Session::open_any(&path).is_err());
        assert!(
            !path.exists(),
            "a failed import must not leave a file behind"
        );
    }

    #[test]
    fn opening_a_kame_path_still_opens_a_document() {
        let path = crate::test_temp_dir().join(format!("kame-any-{}.kame", uuid::Uuid::new_v4()));
        let (session, note) = Session::open_any(&path).unwrap();
        assert!(!session.is_anonymous());
        assert!(note.is_none());
        drop(session);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fresh_file_creates_a_document() {
        let path = temp_db_path();
        let session = open(&path);
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(session.document().blocks[0].text, "");
        assert_eq!(session.document().title, "doc");
        assert_eq!(session.caret.offset, 0);
        assert!(path.exists(), "opening creates the .kame file");
    }

    #[test]
    fn a_blank_session_surfaces_the_error_and_stays_editable() {
        let path = temp_db_path();
        let mut session = Session::blank(&path, "could not open — corrupt".into()).unwrap();
        assert_eq!(
            session.last_error.as_deref(),
            Some("could not open — corrupt"),
            "the reason is shown in the GUI, not just the console"
        );
        assert_eq!(session.document().blocks.len(), 1);
        assert!(session.insert("scratch"), "the blank document is editable");
        assert_eq!(session.document().blocks[0].text, "scratch");
        assert!(!path.exists(), "a blank session never creates the bad path");
    }

    #[test]
    fn an_anonymous_session_starts_untitled_with_no_path() {
        let mut session = Session::anonymous().unwrap();
        assert!(session.path().is_none());
        assert!(session.is_anonymous());
        assert_eq!(session.document().title, "Untitled");
        assert!(
            !session.has_content(),
            "a fresh Untitled has nothing to save"
        );

        assert!(
            session.insert("scratch"),
            "the anonymous document is editable"
        );
        assert_eq!(session.document().blocks[0].text, "scratch");
        assert!(session.has_content(), "prose is worth saving");
    }

    #[test]
    fn save_as_persists_the_document_and_its_history() {
        let mut session = Session::anonymous().unwrap();
        session.insert("Hello");
        session.insert(" world");
        assert_eq!(session.history(), (2, 0));

        let path = temp_db_path();
        assert!(!path.exists());
        session.save_as(&path).unwrap();

        assert_eq!(session.path(), Some(path.as_path()));
        assert!(!session.is_anonymous(), "the document now has a file");
        assert!(path.exists(), "save-as wrote the file");

        // The file round-trips: same text, and the undo history travelled too.
        drop(session);
        let reopened = open(&path);
        assert_eq!(reopened.document().blocks[0].text, "Hello world");
        assert_eq!(reopened.history(), (2, 0));
    }

    #[test]
    fn link_target_prefers_selection_then_caret_link() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("click here");
        let id = first_block_id(&session);

        // Nothing selected, no link: no target.
        assert!(session.link_target().is_none());

        // A caret inside a link (no selection) edits that link, pre-filling its URL.
        session.set_link(id, 0, 5, "http://a.com".into());
        session.set_caret(Cursor {
            block_id: id,
            offset: 2,
        });
        assert_eq!(
            session.link_target(),
            Some((id, 0, 5, Some("http://a.com".to_string())))
        );

        // A selection over "here" becomes the target with no pre-fill.
        session.set_caret(Cursor {
            block_id: id,
            offset: 6,
        });
        session.select_to(Cursor {
            block_id: id,
            offset: 10,
        });
        assert_eq!(session.link_target(), Some((id, 6, 10, None)));
    }

    #[test]
    fn typing_persists_and_the_caret_advances() {
        let path = temp_db_path();
        let mut session = open(&path);
        assert!(session.insert("Hello"));
        assert!(session.insert(" world"));
        assert_eq!(session.document().blocks[0].text, "Hello world");
        assert_eq!(session.caret.offset, 11);
        assert_eq!(session.history(), (2, 0));
        drop(session);

        let mut reopened = open(&path);
        assert_eq!(reopened.document().blocks[0].text, "Hello world");
        assert_eq!(reopened.history(), (2, 0));
        // Undo after reopening must undo one insert, not re-apply the log.
        assert!(reopened.undo());
        assert_eq!(reopened.document().blocks[0].text, "Hello");
        assert_eq!(reopened.history(), (1, 1));
    }

    #[test]
    fn split_creates_a_paragraph_and_puts_the_caret_in_it() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("one two");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 3,
        });
        assert!(session.split());

        assert_eq!(session.document().blocks.len(), 2);
        assert_eq!(session.document().blocks[0].text, "one");
        assert_eq!(session.document().blocks[1].text, " two");
        assert_eq!(session.caret.block_id, session.document().blocks[1].id);
        assert_eq!(session.caret.offset, 0);

        session.insert("new");
        assert_eq!(session.document().blocks[1].text, "new two");
        assert_eq!(session.caret.offset, 3);
        drop(session);

        let reopened = open(&path);
        assert_eq!(reopened.document().blocks.len(), 2);
        assert_eq!(reopened.document().blocks[1].text, "new two");
    }

    #[test]
    fn backspace_at_a_block_start_joins_the_previous_paragraph() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("first");
        session.split();
        session.insert("second");
        assert_eq!(session.document().blocks.len(), 2);
        assert_eq!(session.caret.offset, 6);

        assert!(session.backspace());
        assert_eq!(session.document().blocks[1].text, "secon");
        assert_eq!(session.caret.offset, 5);

        session.set_caret(Cursor {
            block_id: session.caret.block_id,
            offset: 0,
        });
        assert!(session.backspace());
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(session.document().blocks[0].text, "firstsecon");
        assert_eq!(session.caret.offset, 5, "caret lands at the join");
        assert_eq!(session.caret.block_id, first_block_id(&session));
    }

    #[test]
    fn delete_at_a_block_end_pulls_the_next_paragraph_up() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("one");
        session.split();
        session.insert("two");
        session.set_caret(Cursor {
            block_id: first_block_id(&session),
            offset: 3,
        });
        assert!(session.delete_forward());
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(session.document().blocks[0].text, "onetwo");
        assert_eq!(session.caret.offset, 3);
    }

    #[test]
    fn backspace_deletes_a_whole_utf8_character() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("aé");
        assert_eq!(session.caret.offset, 3);
        assert!(session.backspace());
        assert_eq!(session.document().blocks[0].text, "a");
        assert_eq!(session.caret.offset, 1);
    }

    #[test]
    fn undo_and_redo_persist_head_and_cursor_across_a_reopen() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("ab");
        session.insert("cd");
        assert!(session.undo());
        assert_eq!(session.document().blocks[0].text, "ab");
        drop(session);

        let mut reopened = open(&path);
        assert_eq!(reopened.document().blocks[0].text, "ab");
        assert_eq!(reopened.history(), (1, 1));
        assert!(reopened.redo());
        assert_eq!(reopened.document().blocks[0].text, "abcd");
        assert_eq!(reopened.history(), (2, 0));
    }

    #[test]
    fn paste_splits_crlf_text_into_paragraphs() {
        let path = temp_db_path();
        let mut session = open(&path);
        assert!(session.paste("one\r\ntwo\r\nthree"));
        let texts: Vec<&str> = session
            .document()
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect();
        assert_eq!(texts, vec!["one", "two", "three"]);
        assert_eq!(session.caret.offset, 5);
        assert_eq!(session.caret.block_id, session.document().blocks[2].id);
    }

    #[test]
    fn arrows_cross_block_boundaries() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("ab");
        session.split();
        session.insert("cd");
        session.set_caret(Cursor {
            block_id: session.caret.block_id,
            offset: 0,
        });
        session.move_left(false);
        assert_eq!(session.caret.offset, 2);
        assert_eq!(session.caret.block_id, first_block_id(&session));
        session.move_right(false);
        assert_eq!(session.caret.offset, 0);
        assert_eq!(session.caret.block_id, session.document().blocks[1].id);
    }

    #[test]
    fn shift_arrow_extends_the_selection() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("word");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 4,
        });

        // Shift-Left from the end selects the previous character, anchored at
        // the start (offset 4).
        session.move_left(true);
        assert_eq!(
            session.ordered_selection(),
            Some((
                Cursor {
                    block_id,
                    offset: 3
                },
                Cursor {
                    block_id,
                    offset: 4
                },
            ))
        );

        // Shift-Right back onto the anchor collapses the selection to nothing.
        session.move_right(true);
        assert_eq!(session.ordered_selection(), None);
    }

    #[test]
    fn clamping_repairs_a_stale_caret_after_undo() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("abc");
        session.split();
        let second = session.document().blocks[1].id;
        session.insert("ghost");
        session.set_caret(Cursor {
            block_id: second,
            offset: 5,
        });
        assert!(session.undo()); // removes "ghost"
        assert_eq!(session.caret.block_id, second);
        assert_eq!(session.caret.offset, 0, "offset clamps to the empty block");
        // Undo the split as well: the block disappears and the caret moves home.
        assert!(session.undo());
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(session.caret.block_id, first_block_id(&session));
        assert_eq!(session.caret.offset, 0);
    }

    #[test]
    fn typing_after_an_inserted_step_stays_a_step_apart() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("see ");
        assert!(session.insert_as_step("[é]"));
        for typed in ["n", "o", "w"] {
            assert!(session.insert(typed));
        }
        assert_eq!(session.document().blocks[0].text, "see [é]now");
        assert_eq!(session.history(), (3, 0));

        // Neither a reopen nor compacting the history joins them, as both work from the log.
        drop(session);
        let mut session = open(&path);
        assert_eq!(session.history(), (3, 0));
        assert_eq!(session.compact_history(), Ok((3, 3)));
        assert!(session.undo());
        assert_eq!(session.document().blocks[0].text, "see [é]");
        assert_eq!(session.caret.offset, "see [é]".len());
        assert!(session.undo());
        assert_eq!(session.document().blocks[0].text, "see ");
        assert_eq!(session.caret.offset, "see ".len());
        assert!(session.redo());
        assert_eq!(session.document().blocks[0].text, "see [é]");
        assert_eq!(session.caret.offset, "see [é]".len());
        assert!(session.redo());
        assert_eq!(session.document().blocks[0].text, "see [é]now");
        assert_eq!(session.caret.offset, "see [é]now".len());
    }

    #[test]
    fn typing_after_a_paste_stays_a_step_apart() {
        let mut session = Session::anonymous().unwrap();
        session.insert("see ");
        assert!(session.paste("one two"));
        assert!(session.insert("!"));
        assert_eq!(session.document().blocks[0].text, "see one two!");
        assert!(session.undo());
        assert_eq!(session.document().blocks[0].text, "see one two");

        // The last line of a paste over several blocks is apart from what is typed after it.
        assert!(session.paste("a\nbc"));
        assert!(session.insert("d"));
        assert!(session.undo());
        assert_eq!(session.document().blocks[1].text, "bc");
        assert!(session.paste_clipped("xy", None));
        assert!(session.insert("z"));
        assert!(session.undo());
        assert_eq!(session.document().blocks[1].text, "bcxy");
    }

    #[test]
    fn an_inserted_step_widens_a_note_it_lands_inside() {
        let mut session = Session::anonymous().unwrap();
        session.insert("one two");
        let block_id = session.document().blocks[0].id;
        session.set_caret(Cursor {
            block_id,
            offset: 2,
        });
        session.add_annotation(AnnotationKind::Note).unwrap();
        let before = session.document().clone();
        let (start, end) = before.annotations[0].anchor.byte_range().unwrap();
        assert_eq!((start, end), (0, 3));
        session.set_caret(Cursor {
            block_id,
            offset: 2,
        });
        assert!(session.insert_as_step("[k]"));
        assert_eq!(
            session.document().annotations[0].anchor.byte_range(),
            Some((0, 6))
        );
        assert!(session.undo());
        assert_eq!(session.document(), &before);
    }

    /// FIX 1: undo/redo derive the caret from the op they applied, so typing
    /// after Ctrl-Z inserts where the erased text was.
    #[test]
    fn undo_and_redo_move_the_caret_to_the_affected_span() {
        let path = temp_db_path();
        let mut session = open(&path);
        let block_id = first_block_id(&session);
        session.insert("abc");
        session.set_caret(Cursor {
            block_id,
            offset: 1,
        });
        session.insert("XY");
        assert_eq!(session.document().blocks[0].text, "aXYbc");
        assert_eq!(session.caret.offset, 3);

        assert!(session.undo());
        assert_eq!(session.document().blocks[0].text, "abc");
        assert_eq!(
            session.caret.offset, 1,
            "undo parks the caret at the start of the erased span"
        );

        assert!(session.redo());
        assert_eq!(session.document().blocks[0].text, "aXYbc");
        assert_eq!(
            session.caret.offset, 3,
            "redo parks the caret after the reinserted text"
        );
    }

    /// FIX 4: a failed write makes the session read-only instead of letting
    /// memory and DB history diverge; the flag clears on the next good write.
    #[test]
    fn a_save_failure_suppresses_edits_until_a_write_succeeds() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("abc");
        let block_id = first_block_id(&session);
        assert_eq!(session.history(), (1, 0));

        // Simulate the state a failed record_edit leaves behind.
        session.save_failed = true;
        session.last_error = Some("save failed — editing disabled: disk full".into());

        assert!(!session.insert("d"), "insert is refused while save failed");
        assert!(!session.backspace());
        assert!(!session.split());
        assert!(!session.undo());
        assert_eq!(session.document().blocks[0].text, "abc");
        assert_eq!(session.history(), (1, 0), "history did not advance");
        assert!(session.last_error.is_some(), "the error stays visible");

        // Movement is not a mutation and is still allowed.
        session.set_caret(Cursor {
            block_id,
            offset: 3,
        });
        session.move_left(false);
        assert_eq!(session.caret.offset, 2);
        session.set_caret(Cursor {
            block_id,
            offset: 3,
        });

        // A successful write resolves the failure and re-enables editing.
        assert!(session.save_state());
        assert!(!session.save_failed);
        assert!(session.last_error.is_none());
        assert!(session.insert("d"));
        assert_eq!(session.document().blocks[0].text, "abcd");
    }

    #[test]
    fn a_block_kind_change_commits_as_an_undoable_edit() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("Title");
        let block_id = first_block_id(&session);
        assert_eq!(session.document().blocks[0].kind, BlockKind::Paragraph);

        assert!(session.set_block_kind(block_id, BlockKind::Heading { level: 1 }));
        assert_eq!(
            session.document().blocks[0].kind,
            BlockKind::Heading { level: 1 }
        );
        assert_eq!(session.history(), (2, 0), "the change is one undoable op");

        // Undo restores the previous kind: the op carries it.
        assert!(session.undo());
        assert_eq!(session.document().blocks[0].kind, BlockKind::Paragraph);
        assert!(session.redo());

        // The kind is persisted with the head document, so a reopen sees it.
        drop(session);
        let reopened = open(&path);
        assert_eq!(
            reopened.document().blocks[0].kind,
            BlockKind::Heading { level: 1 }
        );
    }

    #[test]
    fn an_unreadable_op_row_degrades_to_a_writable_session() {
        let path = temp_db_path();
        {
            let mut session = open(&path);
            assert!(session.insert("kept"));
        }
        // A row serde cannot read, but not one from a newer build, which
        // would be a kind of step this build doesn't know: this one is missing
        // what its kind needs, as from a half-written row.
        {
            let conn = rusqlite::Connection::open(&path).expect("open the db directly");
            conn.execute(
                "INSERT INTO ops (json) VALUES ('{\"SetTitle\":{\"old\":\"a\"}}')",
                [],
            )
            .expect("write a bad op row");
        }

        let mut session = open(&path);
        assert_eq!(
            session.document().blocks[0].text,
            "kept",
            "the head document opens even when history does not"
        );
        assert_eq!(session.history(), (0, 0), "history is unavailable");
        let warning = session.last_error.clone().expect("a warning is shown");
        assert!(
            warning.contains("history unavailable"),
            "the warning names the degradation, got {warning:?}"
        );
        assert!(!session.save_failed, "a bad read is not a failed write");

        // The log cannot be resumed from, and leaving it beside an editor with
        // no history desyncs the two: the first edit truncates the log as a
        // redo tail (shifting the sequence numbers the cursor counts), the next
        // undo writes the in-memory count as the cursor, and a reopen offers a
        // surviving op as redo against text it never touched. So the open
        // resets the durable state to the degraded editor: empty log, cursor 0.
        {
            let conn = rusqlite::Connection::open(&path).expect("open the db directly");
            let rows: i64 = conn
                .query_row("SELECT COUNT(*) FROM ops", [], |row| row.get(0))
                .expect("count ops");
            assert_eq!(rows, 0, "the unresumable log is dropped with the reset");
            let cursor: String = conn
                .query_row(
                    "SELECT value FROM meta WHERE key = 'undo_cursor'",
                    [],
                    |row| row.get(0),
                )
                .expect("read the cursor");
            assert_eq!(cursor, "0", "the cursor no longer points into the log");
        }

        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 4,
        });
        assert!(session.insert("!"), "the session stays writable");
        assert_eq!(session.document().blocks[0].text, "kept!");
        assert_eq!(session.history(), (1, 0));
        assert!(session.undo(), "the new edit is undoable in this session");
        assert_eq!(session.document().blocks[0].text, "kept");
        drop(session);

        // The regression this guards: with the DB left as it was, the first edit
        // truncated the log under a cursor that still counted the pre-degrade
        // op, and the undo then wrote the in-memory count, so the reopen put the
        // pre-degrade op on the redo stack and redo re-applied it to text that
        // already contained it ("kept" -> "keptkept"). After the reset the log
        // holds only the edit made since the degrade, so the only thing redo can
        // re-apply is the author's own undone edit.
        let mut reopened = open(&path);
        assert_eq!(reopened.document().blocks[0].text, "kept");
        assert_eq!(
            reopened.history(),
            (0, 1),
            "the log holds one post-degrade op and no older one"
        );
        assert!(reopened.redo(), "the author's own undone edit is redoable");
        assert_eq!(
            reopened.document().blocks[0].text,
            "kept!",
            "redo re-applies the typed edit exactly once, never the pre-degrade op"
        );
        assert!(!reopened.redo(), "nothing else is on the redo stack");
        assert_eq!(reopened.document().blocks[0].text, "kept!");
    }

    /// FIX 3: a message that has no pending write (the degraded-open warning, a
    /// failed export) is dismissed by the strip's click, not retried: retrying
    /// writes state the author never changed and never clears the message.
    #[test]
    fn a_message_with_no_pending_write_is_dismissed_not_retried() {
        let path = temp_db_path();
        let mut session = open(&path);
        assert!(session.insert("draft"));

        session.last_error = Some("export failed — no such directory".into());
        assert!(
            !session.save_failed,
            "the export failure is not a save failure"
        );
        assert!(session.retry_or_dismiss(), "a click clears the message");
        assert!(session.last_error.is_none(), "the strip goes away");
        assert!(!session.save_failed);
        assert_eq!(session.history(), (1, 0), "nothing was rewritten");

        // A failed write is the other class: the same click retries it, and it
        // stays until a write lands.
        session.save_failed = true;
        session.last_error = Some("save failed — editing disabled: disk full".into());
        assert!(session.retry_or_dismiss(), "the retry lands");
        assert!(!session.save_failed);
        assert!(session.last_error.is_none());
    }

    #[test]
    fn adding_a_note_at_a_caret_anchors_its_word_and_persists() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("The quick brown fox");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 6,
        });

        let id = session
            .add_annotation(AnnotationKind::Note)
            .expect("a note over the word at the caret");
        assert_eq!(session.document().annotations.len(), 1);
        let annotation = &session.document().annotations[0];
        assert_eq!(annotation.id, id);
        assert_eq!(annotation.kind, AnnotationKind::Note);
        assert_eq!(
            annotation.anchor,
            Anchor::range(block_id, 4, 9),
            "a caret anchors the word around it"
        );
        assert_eq!(annotation.body, "", "a new note starts as a bare highlight");
        drop(session);

        let reopened = open(&path);
        assert_eq!(reopened.document().annotations.len(), 1);
        assert_eq!(
            reopened.document().annotations[0]
                .anchor
                .byte_range()
                .unwrap()
                .0,
            4
        );
    }

    #[test]
    fn removing_a_note_clears_its_highlight_and_undo_restores_it() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("The quick brown fox");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 6,
        });
        let id = session
            .add_annotation(AnnotationKind::Note)
            .expect("a note to remove");

        assert!(session.remove_annotation(id), "the removal persists");
        assert!(
            session.document().annotations.is_empty(),
            "the highlight is gone"
        );

        session.undo();
        assert_eq!(
            session.document().annotations.len(),
            1,
            "undo restores the note and its highlight"
        );
    }

    #[test]
    fn marginalia_anchor_to_the_block_and_notes_to_the_selection() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("The quick brown fox");
        let block_id = first_block_id(&session);

        // Drag backwards: the anchor still comes out in document order.
        session.set_caret(Cursor {
            block_id,
            offset: 15,
        });
        session.select_to(Cursor {
            block_id,
            offset: 4,
        });

        // A marginal anchors to the whole block, selection or not.
        let id = session
            .add_annotation(AnnotationKind::Marginal)
            .expect("a marginal over the selection");
        let annotation = session
            .document()
            .annotations
            .iter()
            .find(|annotation| annotation.id == id)
            .expect("the new annotation");
        assert_eq!(annotation.kind, AnnotationKind::Marginal);
        assert_eq!(
            annotation.anchor,
            Anchor::block(block_id),
            "marginalia anchor to the whole block, not the selection"
        );

        // A note anchors to the selected range, in document order.
        let id = session
            .add_annotation(AnnotationKind::Note)
            .expect("a note over the selection");
        let annotation = session
            .document()
            .annotations
            .iter()
            .find(|annotation| annotation.id == id)
            .expect("the new note");
        assert_eq!(annotation.kind, AnnotationKind::Note);
        assert_eq!(
            annotation.anchor,
            Anchor::range(block_id, 4, 15),
            "the selected bytes become the anchor, never reversed"
        );
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("hello world");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 6,
        });
        session.select_to(Cursor {
            block_id,
            offset: 11,
        });

        assert!(session.insert("there"));
        assert_eq!(session.document().blocks[0].text, "hello there");
        assert_eq!(session.caret.offset, 11);
        assert!(session.selection.is_none());
    }

    #[test]
    fn backspace_over_a_selection_deletes_the_range() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("hello world");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 5,
        });
        session.select_to(Cursor {
            block_id,
            offset: 11,
        });

        assert!(session.backspace());
        assert_eq!(session.document().blocks[0].text, "hello");
        assert_eq!(session.caret.offset, 5);
    }

    #[test]
    fn a_note_anchor_survives_typing_above_it() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("The quick brown fox");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 6,
        });
        let id = session
            .add_annotation(AnnotationKind::Footnote)
            .expect("a footnote on 'quick'");

        // Type at the very start: every later anchor rebases by the insert.
        session.set_caret(Cursor {
            block_id,
            offset: 0,
        });
        assert!(session.insert("Very "));

        let annotation = session
            .document()
            .annotations
            .iter()
            .find(|annotation| annotation.id == id)
            .expect("the footnote survives the edit");
        assert_eq!(
            annotation.anchor,
            Anchor::range(block_id, 9, 14),
            "the anchor moved with its word, keeping the same range"
        );
    }

    #[test]
    fn annotation_add_is_refused_after_a_failed_save() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("abc");
        session.save_failed = true;
        session.last_error = Some("save failed — editing disabled: disk full".into());

        assert!(session.add_annotation(AnnotationKind::Footnote).is_none());
        assert!(session.document().annotations.is_empty());
    }

    #[test]
    fn annotation_bodies_truncate_multibyte_text_on_a_char_boundary() {
        assert_eq!(shorten("abc", 8), "abc");
        assert_eq!(shorten("abcdef", 3), "abc…");
        assert_eq!(shorten("éééé", 2), "éé…");
        assert_eq!(
            default_annotation_body(&AnnotationKind::Footnote, "  word  "),
            "Footnote on “word”"
        );
    }

    /// An authored body is document state, not a display default: it has to
    /// survive close/reopen through the op log, and undo after the reopen must
    /// put the generated body back.
    #[test]
    fn an_authored_body_persists_across_reopen() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("The quick brown fox");
        let block_id = first_block_id(&session);
        session.set_caret(Cursor {
            block_id,
            offset: 6,
        });
        let id = session
            .add_annotation(AnnotationKind::Note)
            .expect("a note on 'quick'");
        let generated = session.document().annotations[0].body.clone();
        assert_eq!(generated, "", "a new note starts as a bare highlight");

        assert!(
            session.set_annotation_body(id, "Show that the fox is not asleep."),
            "the body edit is applied and persisted"
        );
        assert_eq!(
            session.document().annotations[0].body,
            "Show that the fox is not asleep."
        );
        drop(session);

        let mut reopened = open(&path);
        assert_eq!(reopened.document().annotations.len(), 1);
        assert_eq!(
            reopened.document().annotations[0].body,
            "Show that the fox is not asleep.",
            "the authored body comes back from the head document"
        );

        // The edit is in the log, so undo after the reopen restores the body
        // the note was created with.
        assert!(reopened.undo(), "the body edit is undoable after a reopen");
        assert_eq!(reopened.document().annotations[0].body, generated);
        assert!(reopened.redo(), "and redoable");
        assert_eq!(
            reopened.document().annotations[0].body,
            "Show that the fox is not asleep."
        );
    }

    /// The popover commits keystroke by keystroke, so an empty body is a real
    /// state (the author cleared the note) rather than a reference to the
    /// default, and it has to round trip as such. A note with no body is a
    /// plain highlight, so it survives.
    #[test]
    fn clearing_a_note_body_persists_as_an_empty_body() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("word");
        let id = session
            .add_annotation(AnnotationKind::Note)
            .expect("a note on 'word'");

        assert!(session.set_annotation_body(id, ""));
        assert_eq!(session.document().annotations[0].body, "");
        drop(session);

        let reopened = open(&path);
        assert_eq!(reopened.document().annotations[0].body, "");
    }

    /// Clearing a marginal's body removes it: an empty margin entry has
    /// nothing to say, so it goes away instead of lingering.
    #[test]
    fn clearing_a_marginal_body_removes_the_marginal() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("word");
        let id = session
            .add_annotation(AnnotationKind::Marginal)
            .expect("a marginal on 'word'");

        assert!(session.set_annotation_body(id, ""));
        assert!(session.document().annotations.is_empty());
    }

    /// Backspacing the space before a footnote re-anchors the footnote instead
    /// of dropping it: a caret on whitespace anchors a point, and that point
    /// sits on the delete's end boundary.
    #[test]
    fn backspacing_before_a_point_anchored_footnote_reanchors_it() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("a  b");
        let block_id = first_block_id(&session);
        // Caret in the second space: `word_at` returns a point anchor there.
        session.set_caret(Cursor {
            block_id,
            offset: 2,
        });
        let id = session
            .add_annotation(AnnotationKind::Footnote)
            .expect("a footnote on the whitespace");
        assert_eq!(
            session.document().annotations[0].anchor.byte_range(),
            Some((2, 2)),
            "a caret on whitespace anchors a point"
        );

        // Backspace deletes the first space [1, 2); the point at 2 is the
        // delete's end boundary and must close up to 1, not drop.
        assert!(session.backspace());
        assert_eq!(session.document().blocks[0].text, "a b");
        assert_eq!(
            session.document().annotations.len(),
            1,
            "the footnote survives"
        );
        assert_eq!(
            session.document().annotations[0].anchor.byte_range(),
            Some((1, 1)),
            "the point anchor closes up by one"
        );
        assert_eq!(session.document().annotations[0].id, id);
    }

    #[test]
    fn exports_land_beside_the_source_and_fall_back_to_the_cwd() {
        assert_eq!(
            export_path(Path::new("/tmp/notes/draft.kame"), "md"),
            PathBuf::from("/tmp/notes/draft.md")
        );
        assert_eq!(
            export_path(Path::new("/tmp/notes/draft.kame"), "html"),
            PathBuf::from("/tmp/notes/draft.html")
        );
        // A bare name has no directory part, so it resolves in the working
        // directory — the fallback for a session whose path is just a file name.
        assert_eq!(
            export_path(Path::new("draft.kame"), "md"),
            PathBuf::from("draft.md")
        );
        // A path with no stem at all still exports to something nameable.
        assert_eq!(
            export_path(Path::new(".."), "md"),
            PathBuf::from("document.md")
        );
    }

    #[test]
    fn exporting_html_writes_a_page_beside_the_database() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("A paragraph.");
        let expected = crate::export::to_html_with(
            session.document(),
            &session.published_metadata(),
            &References::new(),
        );

        let written = session.export_html().expect("an html export");
        assert_eq!(written, path.with_extension("html"));
        let contents = std::fs::read_to_string(&written).expect("read the export");
        assert_eq!(contents, expected, "the file is exactly to_html's output");
        assert!(contents.starts_with("<!DOCTYPE html>"), "{contents:.40}");
        assert!(contents.contains("<p>A paragraph.</p>"), "{contents}");
    }

    #[test]
    fn exporting_with_references_writes_the_section() {
        let mut references = References::new();
        references.insert(
            "knuth1981".to_string(),
            crate::references::Reference {
                kind: crate::references::Kind::Freeform,
                text: "D. E. Knuth, a paper.".to_string(),
                ..Default::default()
            },
        );
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("Broken [knuth1981].");

        let html = path.with_extension("cited.html");
        assert_eq!(
            session.export_html_to(&html, &references),
            Some(html.clone())
        );
        let contents = std::fs::read_to_string(&html).expect("read the export");
        assert!(contents.contains("<h2>References</h2>"), "{contents}");
        assert!(
            contents.contains("<p id=\"ref-knuth1981\">[knuth1981] D. E. Knuth, a paper.</p>"),
            "{contents}"
        );

        let markdown = path.with_extension("cited.md");
        session.export_markdown_to(&markdown, &references).unwrap();
        let contents = std::fs::read_to_string(&markdown).expect("read the export");
        assert!(
            contents.contains("## References\n\n\\[knuth1981\\] D. E. Knuth, a paper.\n"),
            "{contents}"
        );
    }

    #[test]
    fn a_failed_export_reports_itself_without_locking_the_document() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("some text");
        // A directory where the Markdown file belongs makes the write fail.
        std::fs::create_dir(path.with_extension("md")).unwrap();

        assert_eq!(session.export_markdown(), None);
        let error = session.last_error.as_deref().expect("an export error");
        assert!(error.starts_with("export failed"), "{error}");
        assert!(
            !session.save_failed,
            "an export failure is not a save failure: the document is still stored"
        );
        assert!(session.insert(" more"), "editing carries on");
        assert!(
            session.export_html().is_some(),
            "the other format still writes"
        );
        assert!(
            session.last_error.is_none(),
            "a later successful export clears the old export error"
        );
    }

    #[test]
    fn snapshots_list_newest_first_with_their_labels() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("first draft");
        let first = session.snapshot(Some("  first  ")).expect("a snapshot");
        session.insert(" again");
        let second = session.snapshot(Some("")).expect("a second snapshot");

        let listed = session.snapshots().expect("list the snapshots");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, second);
        assert_eq!(listed[1].id, first);
        assert_eq!(
            listed[1].label.as_deref(),
            Some("first"),
            "labels are trimmed and kept"
        );
        assert_eq!(listed[0].label, None, "an empty label stores no label");
    }

    #[test]
    fn restoring_a_snapshot_drops_the_edits_made_after_it() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("the version I kept");
        let id = session.snapshot(Some("kept")).expect("a snapshot");
        session.insert(" and the text I regret");
        assert!(session.document().blocks[0].text.ends_with("regret"));

        assert!(session.restore(id));
        assert_eq!(session.document().blocks[0].text, "the version I kept");
        // The snapshot is the undo point, so the restore is not itself an edit:
        // there is nothing to undo afterwards.
        assert_eq!(session.history(), (0, 0));
        assert!(!session.undo(), "the restore is not undone by Ctrl-Z");
        drop(session);

        // The head, not just the view, moved: reopen sees the restored text.
        let mut reopened = open(&path);
        assert_eq!(reopened.document().blocks[0].text, "the version I kept");
        assert_eq!(reopened.history(), (0, 0), "the old op log was dropped");
        assert_eq!(
            reopened.db.load_ops().unwrap().len(),
            0,
            "the log describes a document that no longer exists"
        );
        assert_eq!(
            reopened.snapshots().unwrap().len(),
            1,
            "restoring keeps the snapshots it read from"
        );
        // The restored document is a clean start for new edits.
        assert!(reopened.insert("!"));
        assert_eq!(reopened.history(), (1, 0));
    }

    #[test]
    fn restoring_a_missing_snapshot_leaves_the_document_alone() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("still here");

        assert!(!session.restore(9_999), "there is no such snapshot");
        assert_eq!(session.document().blocks[0].text, "still here");
        let error = session.last_error.as_deref().expect("a restore error");
        assert!(error.starts_with("restore failed"), "{error}");
        assert!(!session.save_failed, "a failed read is not a failed write");
        assert!(session.insert("!"), "editing carries on");
    }

    fn typed(session: &mut Session, text: &str) {
        for c in text.chars() {
            session.insert(&c.to_string());
            session.apply_markdown_shortcut();
        }
    }

    fn only_text(session: &Session, index: usize) -> (BlockKind, String) {
        let block = &session.document().blocks[index];
        (block.kind.clone(), block.text.clone())
    }

    #[test]
    fn typing_a_bar_and_space_starts_a_prose_block() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "| Roses");
        assert_eq!(
            only_text(&session, 0),
            (BlockKind::Prose, "Roses".to_string())
        );
    }

    #[test]
    fn a_bare_fence_has_no_language() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "```");
        session.split();
        assert_eq!(
            session.document().blocks[0].kind,
            BlockKind::Code { language: None }
        );
    }

    #[test]
    fn enter_adds_a_line_in_a_verbatim_block() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "| one");
        session.split();
        typed(&mut session, "two");
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(
            only_text(&session, 0),
            (BlockKind::Prose, "one\ntwo".to_string())
        );
        assert_eq!(session.caret.offset, "one\ntwo".len());
    }

    #[test]
    fn enter_on_a_blank_last_line_returns_to_body_text() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "| one");
        session.split();
        session.split();
        assert_eq!(session.document().blocks.len(), 2);
        assert_eq!(
            only_text(&session, 0),
            (BlockKind::Prose, "one".to_string())
        );
        assert_eq!(
            only_text(&session, 1),
            (BlockKind::Paragraph, String::new())
        );
        assert_eq!(session.caret.block_id, session.document().blocks[1].id);
        assert_eq!(session.caret.offset, 0);
    }

    #[test]
    fn shift_enter_keeps_blank_lines_without_leaving() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "| one");
        session.line_break();
        session.line_break();
        session.line_break();
        typed(&mut session, "two");
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(
            only_text(&session, 0),
            (BlockKind::Prose, "one\n\n\ntwo".to_string())
        );
    }

    #[test]
    fn enter_in_the_middle_of_a_verbatim_block_only_adds_a_line() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "| one");
        session.line_break();
        session.line_break();
        typed(&mut session, "two");
        session.caret.offset = 4; // the blank line
        session.split();
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(session.document().blocks[0].text, "one\n\n\ntwo");
    }

    #[test]
    fn enter_on_an_empty_verbatim_block_returns_it_to_body_text() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "| ");
        session.split();
        assert_eq!(
            only_text(&session, 0),
            (BlockKind::Paragraph, String::new())
        );
    }

    #[test]
    fn leaving_a_verbatim_block_is_one_undo_step() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "| one");
        session.split();
        session.split();
        session.undo();
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(
            only_text(&session, 0),
            (BlockKind::Prose, "one\n".to_string())
        );
    }

    #[test]
    fn three_dashes_and_enter_make_a_rule_and_a_new_paragraph() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "---");
        session.split();
        assert_eq!(session.document().blocks.len(), 2);
        assert_eq!(only_text(&session, 0), (BlockKind::Rule, String::new()));
        assert_eq!(
            only_text(&session, 1),
            (BlockKind::Paragraph, String::new())
        );
        assert_eq!(session.caret.block_id, session.document().blocks[1].id);
        session.undo();
        assert_eq!(
            only_text(&session, 0),
            (BlockKind::Paragraph, "---".to_string())
        );
    }

    #[test]
    fn typing_on_a_rule_starts_a_paragraph_after_it() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "---");
        session.split();
        let rule = session.document().blocks[0].id;
        session.caret = Cursor {
            block_id: rule,
            offset: 0,
        };
        session.insert("x");
        assert_eq!(only_text(&session, 0), (BlockKind::Rule, String::new()));
        assert_eq!(session.document().blocks[1].text, "x");
    }

    /// The caret stays put when Enter changes the block rather than adding one, wherever
    /// the block is in the document.
    #[test]
    fn leaving_an_empty_list_item_keeps_the_caret_on_it() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "first");
        session.split();
        typed(&mut session, "second");
        session.caret = Cursor {
            block_id: session.document().blocks[0].id,
            offset: "first".len(),
        };
        session.split();
        typed(&mut session, "- ");
        let item = session.caret.block_id;
        assert_eq!(session.document().blocks[1].text, "");
        session.split();
        assert_eq!(session.document().blocks.len(), 3);
        assert_eq!(
            session.caret.block_id, item,
            "the caret jumped to the next block"
        );
    }

    #[test]
    fn an_older_document_reads_as_before() {
        let kind: BlockKind = serde_json::from_str(r#"{"Code":{}}"#).unwrap();
        assert_eq!(kind, BlockKind::Code { language: None });
    }

    fn long_document(session: &mut Session) {
        typed(session, "first");
        session.split();
        typed(session, "second paragraph");
    }

    #[test]
    fn a_document_reopens_where_it_was_left() {
        let path = temp_db_path();
        let mut session = open(&path);
        long_document(&mut session);
        let second = session.document().blocks[1].id;
        session.caret = Cursor {
            block_id: second,
            offset: 6,
        };
        session.save_position(840.0);
        drop(session);

        let reopened = open(&path);
        assert_eq!(reopened.caret.block_id, second);
        assert_eq!(reopened.caret.offset, 6);
        assert_eq!(reopened.restored_scroll, Some(840.0));
    }

    #[test]
    fn a_new_file_has_no_position_to_restore() {
        let session = open(&temp_db_path());
        assert_eq!(session.restored_scroll, None);
        assert_eq!(session.caret.offset, 0);
    }

    #[test]
    fn a_position_in_a_block_that_is_gone_falls_back_to_the_start() {
        let path = temp_db_path();
        let mut session = open(&path);
        long_document(&mut session);
        let first = session.document().blocks[0].id;
        session.caret = Cursor {
            block_id: uuid::Uuid::new_v4(),
            offset: 3,
        };
        session.save_position(10.0);
        drop(session);
        let reopened = open(&path);
        assert_eq!(reopened.caret.block_id, first);
        assert_eq!(reopened.caret.offset, 0);
    }

    #[test]
    fn a_position_past_the_end_of_its_block_is_clamped() {
        let path = temp_db_path();
        let mut session = open(&path);
        typed(&mut session, "abc");
        let block_id = session.document().blocks[0].id;
        session.caret = Cursor {
            block_id,
            offset: 3,
        };
        let id = session.db.load_position();
        assert_eq!(id, None);
        session
            .db
            .save_position(&crate::storage::Position {
                block_id,
                offset: 999,
                scroll: 0.0,
            })
            .unwrap();
        drop(session);
        assert_eq!(open(&path).caret.offset, 3);
    }

    #[test]
    fn an_unsaved_document_has_no_position_to_keep() {
        let mut session = Session::anonymous().unwrap();
        session.insert("Words");
        session.save_position(50.0);
        assert_eq!(session.db.load_position(), None);
    }

    #[test]
    fn saving_a_position_is_not_an_edit() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("Words");
        let history = session.history();
        session.save_position(5.0);
        assert_eq!(session.history(), history);
    }

    #[test]
    fn a_position_in_a_block_that_is_gone_restores_no_scroll_either() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.caret.block_id = uuid::Uuid::new_v4();
        session.save_position(300.0);
        drop(session);
        assert_eq!(open(&path).restored_scroll, None);
    }

    fn type_marked(session: &mut Session, text: &str) {
        for c in text.chars() {
            session.insert(&c.to_string());
            session.apply_inline_markdown();
            session.apply_markdown_link();
        }
    }

    fn code_session() -> Session {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "```");
        session.split();
        session
    }

    #[test]
    fn typed_emphasis_stays_literal_in_code() {
        let mut session = code_session();
        type_marked(&mut session, "*a* **b** [c](d)");
        assert_eq!(session.document().blocks[0].text, "*a* **b** [c](d)");
        assert!(session.document().blocks[0].spans.is_empty());
    }

    #[test]
    fn typed_emphasis_formats_in_prose() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "| ");
        type_marked(&mut session, "*a* and **b**");
        let block = &session.document().blocks[0];
        assert_eq!(block.kind, BlockKind::Prose);
        assert_eq!(block.text, "a and b");
        assert_eq!(block.spans.len(), 2);
    }

    #[test]
    fn a_typed_link_stays_literal_in_prose() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "| ");
        type_marked(&mut session, "[c](d)");
        assert_eq!(session.document().blocks[0].text, "[c](d)");
    }

    #[test]
    fn pasted_lines_stay_in_a_verbatim_block() {
        let mut session = code_session();
        assert!(session.paste("fn a() {\r\n    b();\n\n}\n\nfn c() {}"));
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(
            session.document().blocks[0].text,
            "fn a() {\n    b();\n\n}\n\nfn c() {}"
        );
        assert_eq!(
            session.caret.offset,
            session.document().blocks[0].text.len()
        );
        session.undo();
        assert_eq!(
            session.document().blocks[0].text,
            "",
            "one undo takes it all back"
        );
    }

    #[test]
    fn pasted_lines_replace_a_selection_in_a_verbatim_block() {
        let mut session = code_session();
        session.paste("abc");
        let block_id = session.document().blocks[0].id;
        session.selection = Some(Selection {
            start: Cursor {
                block_id,
                offset: 1,
            },
            end: Cursor {
                block_id,
                offset: 2,
            },
        });
        session.paste("x\ny");
        assert_eq!(session.document().blocks[0].text, "ax\nyc");
    }

    #[test]
    fn pasted_lines_still_make_paragraphs_in_body_text() {
        let mut session = Session::anonymous().unwrap();
        session.paste("one\ntwo");
        assert_eq!(session.document().blocks.len(), 2);
    }

    /// Two blocks, with the caret at `offset` in the first.
    fn caret_in_first_of_two(first: &str, offset: usize) -> (Session, BlockId, BlockId) {
        let mut session = Session::anonymous().unwrap();
        session.insert(first);
        session.split();
        session.insert("next");
        let (one, two) = (
            session.document().blocks[0].id,
            session.document().blocks[1].id,
        );
        session.caret = Cursor {
            block_id: one,
            offset,
        };
        (session, one, two)
    }

    #[test]
    fn moving_right_stops_at_the_end_of_a_blocks_last_word() {
        for from in [4, 6] {
            let (mut session, one, two) = caret_in_first_of_two("one two", from);
            session.move_right_word(false);
            assert_eq!((session.caret.block_id, session.caret.offset), (one, 7));
            session.move_right_word(false);
            assert_eq!((session.caret.block_id, session.caret.offset), (two, 0));
        }
    }

    #[test]
    fn moving_right_takes_a_sentences_end_in_one_stop() {
        let (mut session, one, two) = caret_in_first_of_two("one two.”", 4);
        let end = "one two.”".len();
        session.move_right_word(false);
        assert_eq!((session.caret.block_id, session.caret.offset), (one, end));
        session.move_right_word(false);
        assert_eq!((session.caret.block_id, session.caret.offset), (two, 0));
    }

    #[test]
    fn moving_right_reaches_the_end_of_the_last_word_of_the_document() {
        let mut session = Session::anonymous().unwrap();
        session.insert("one two");
        session.caret.offset = 4;
        session.move_right_word(false);
        assert_eq!(session.caret.offset, 7);
        session.move_right_word(false);
        assert_eq!(session.caret.offset, 7, "there is nowhere further to go");
    }

    #[test]
    fn selecting_right_takes_just_the_last_word() {
        let (mut session, _, _) = caret_in_first_of_two("one two", 4);
        session.move_right_word(true);
        assert_eq!(session.selected_text().as_deref(), Some("two"));
    }

    #[test]
    fn moving_right_to_a_last_words_end_counts_bytes() {
        let (mut session, one, _) = caret_in_first_of_two("un été", 3);
        session.move_right_word(false);
        assert_eq!(
            (session.caret.block_id, session.caret.offset),
            (one, "un été".len())
        );
    }

    #[test]
    fn deleting_a_word_back_takes_the_word_before_the_caret() {
        let mut session = Session::anonymous().unwrap();
        session.insert("one two");
        assert!(session.delete_word_back());
        assert_eq!(session.document().blocks[0].text, "one ");
        assert_eq!(session.caret.offset, 4);
        session.undo();
        assert_eq!(session.document().blocks[0].text, "one two");
    }

    #[test]
    fn deleting_a_word_back_counts_bytes_not_characters() {
        let mut session = Session::anonymous().unwrap();
        session.insert("één élan");
        assert!(session.delete_word_back());
        assert_eq!(session.document().blocks[0].text, "één ");
    }

    #[test]
    fn deleting_a_word_back_at_a_block_start_joins_the_blocks() {
        let mut session = Session::anonymous().unwrap();
        session.insert("one");
        session.split();
        session.insert("two");
        session.caret.offset = 0;
        assert!(session.delete_word_back());
        assert_eq!(session.document().blocks.len(), 1);
        assert_eq!(session.document().blocks[0].text, "onetwo");
    }

    #[test]
    fn deleting_a_word_back_with_a_selection_deletes_the_selection() {
        let mut session = Session::anonymous().unwrap();
        session.insert("one two");
        let block_id = session.document().blocks[0].id;
        session.selection = Some(Selection {
            start: Cursor {
                block_id,
                offset: 1,
            },
            end: Cursor {
                block_id,
                offset: 2,
            },
        });
        assert!(session.delete_word_back());
        assert_eq!(session.document().blocks[0].text, "oe two");
    }

    #[test]
    fn the_document_edges_are_the_first_and_last_blocks() {
        let mut session = Session::anonymous().unwrap();
        session.insert("one");
        session.split();
        session.insert("second block");
        let blocks = &session.document().blocks;
        assert_eq!(
            session.document_edge(false),
            Some(Cursor {
                block_id: blocks[0].id,
                offset: 0
            })
        );
        assert_eq!(
            session.document_edge(true),
            Some(Cursor {
                block_id: blocks[1].id,
                offset: "second block".len()
            })
        );
    }

    #[test]
    fn a_typed_tab_lands_in_a_code_block() {
        let mut session = code_session();
        session.insert("\t");
        assert_eq!(session.document().blocks[0].text, "\t");
    }

    fn document_link_spans(session: &Session) -> Vec<(usize, usize, String)> {
        session.document().blocks[0]
            .spans
            .iter()
            .filter_map(|span| match &span.style {
                SpanStyle::DocumentLink { target } => Some((span.start, span.end, target.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_typed_document_link_shows_its_text_and_keeps_its_target() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "see [[notes/my_a_b#The Intro|the intro]] now");
        assert_eq!(session.document().blocks[0].text, "see the intro now");
        assert_eq!(
            document_link_spans(&session),
            vec![(4, 13, "notes/my_a_b#The Intro".to_string())]
        );
    }

    #[test]
    fn a_typed_document_link_without_text_shows_its_target() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "[[essay]]");
        assert_eq!(session.document().blocks[0].text, "essay");
        assert_eq!(
            document_link_spans(&session),
            vec![(0, 5, "essay".to_string())]
        );
        assert_eq!(session.caret.offset, 5);
    }

    #[test]
    fn a_typed_document_link_counts_bytes() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "é [[été|été]]");
        assert_eq!(session.document().blocks[0].text, "é été");
        let start = "é ".len();
        assert_eq!(
            document_link_spans(&session),
            vec![(start, start + "été".len(), "été".to_string())]
        );
    }

    #[test]
    fn a_typed_document_link_is_one_undo_step_back_to_what_was_typed() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "[[a|b]]");
        session.undo();
        assert_eq!(session.document().blocks[0].text, "[[a|b]]");
        assert!(session.document().blocks[0].spans.is_empty());
    }

    #[test]
    fn incomplete_document_links_stay_as_typed() {
        for typed in ["[[]]", "[[|x]]", "[[x|]]", "[a]]", "a]]"] {
            let mut session = Session::anonymous().unwrap();
            type_marked(&mut session, typed);
            assert_eq!(session.document().blocks[0].text, typed);
            assert!(session.document().blocks[0].spans.is_empty(), "{typed}");
        }
    }

    #[test]
    fn a_document_link_stays_literal_in_code_and_prose() {
        let mut code = code_session();
        type_marked(&mut code, "[[a|b]]");
        assert_eq!(code.document().blocks[0].text, "[[a|b]]");
        let mut prose = Session::anonymous().unwrap();
        typed(&mut prose, "| ");
        type_marked(&mut prose, "[[a|b]]");
        assert_eq!(prose.document().blocks[0].text, "[[a|b]]");
    }

    #[test]
    fn an_ordinary_link_to_a_kame_file_becomes_a_document_link() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "[essay](notes/a.kame#Intro)");
        assert_eq!(
            document_link_spans(&session),
            vec![(0, 5, "notes/a.kame#Intro".to_string())]
        );
    }

    #[test]
    fn the_link_prompt_edits_and_removes_a_document_link() {
        let mut session = Session::anonymous().unwrap();
        session.insert("essay");
        let id = session.document().blocks[0].id;
        session.set_link(id, 0, 5, "a.kame".into());
        assert_eq!(
            document_link_spans(&session),
            vec![(0, 5, "a.kame".to_string())]
        );
        session.set_caret(Cursor {
            block_id: id,
            offset: 2,
        });
        assert_eq!(
            session.link_target(),
            Some((id, 0, 5, Some("a.kame".to_string())))
        );
        session.set_link(id, 0, 5, "https://example.com".into());
        assert!(document_link_spans(&session).is_empty());
        assert_eq!(session.document().blocks[0].spans.len(), 1);
        session.set_link(id, 0, 5, "b.kame".into());
        assert_eq!(session.document().blocks[0].spans.len(), 1);
        session.set_link(id, 0, 5, String::new());
        assert!(session.document().blocks[0].spans.is_empty());
    }

    #[test]
    fn a_heading_is_found_by_exactly_its_text() {
        let mut session = Session::anonymous().unwrap();
        session.insert("The Intro");
        session.split();
        typed(&mut session, "# The Intro");
        let heading = session.document().blocks[1].id;
        assert_eq!(
            session.heading_cursor("The Intro"),
            Some(Cursor {
                block_id: heading,
                offset: 0
            })
        );
        assert_eq!(session.heading_cursor("the intro"), None);
        assert_eq!(session.heading_cursor("The"), None);
    }

    /// Every span has to stay on the text it covered: painting slices the text by them.
    fn assert_spans_fit(session: &Session) {
        for block in &session.document().blocks {
            for span in &block.spans {
                assert!(
                    span.start <= span.end
                        && block.text.is_char_boundary(span.start)
                        && block.text.is_char_boundary(span.end),
                    "span {span:?} doesn't fit {:?}",
                    block.text
                );
            }
        }
    }

    /// Text with `styled` already formatted, and the caret at the very start.
    fn styled_at_the_end(styled: &str) -> Session {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, styled);
        let block_id = session.document().blocks[0].id;
        session.set_caret(Cursor {
            block_id,
            offset: 0,
        });
        session
    }

    #[test]
    fn undoing_a_typed_link_before_other_styles_restores_them_exactly() {
        for typed_link in ["[[a|b]]", "[b](a.kame)", "[b](https://a.b)", "*b*", "**b**"] {
            let mut session = styled_at_the_end("*one* [[two|three]]");
            let before = session.document().blocks[0].clone();
            for c in typed_link.chars() {
                session.insert(&c.to_string());
                session.apply_inline_markdown();
                session.apply_markdown_link();
                assert_spans_fit(&session);
            }
            session.undo();
            assert_spans_fit(&session);
            assert_eq!(
                session.document().blocks[0].text,
                format!("{typed_link}{}", before.text),
                "{typed_link}"
            );
            let shifted: Vec<(usize, usize)> = before
                .spans
                .iter()
                .map(|span| (span.start + typed_link.len(), span.end + typed_link.len()))
                .collect();
            let spans: Vec<(usize, usize)> = session.document().blocks[0]
                .spans
                .iter()
                .map(|span| (span.start, span.end))
                .collect();
            assert_eq!(spans, shifted, "{typed_link}");
        }
    }

    /// Random editing, undoing and redoing never panics, leaves every style on its text,
    /// and undoing everything gets back to the start.
    #[test]
    fn random_edits_and_undos_keep_the_document_sound() {
        for seed in 1..=15u64 {
            let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let mut next = move |limit: usize| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state % limit.max(1) as u64) as usize
            };
            let mut session = Session::anonymous().unwrap();
            let start = session.document().blocks[0].text.clone();
            let mut edits = 0;
            for _ in 0..120 {
                let blocks = session.document().blocks.len();
                let block = session.document().blocks[next(blocks)].clone();
                let mut offset = next(block.text.len() + 1);
                while !block.text.is_char_boundary(offset) {
                    offset -= 1;
                }
                session.set_caret(Cursor {
                    block_id: block.id,
                    offset,
                });
                match next(11) {
                    0 => type_marked(&mut session, "été "),
                    1 => type_marked(&mut session, "[[a_b|c]]"),
                    2 => type_marked(&mut session, "[[essay]]"),
                    3 => type_marked(&mut session, "[d](a.kame#H)"),
                    4 => type_marked(&mut session, "[e](https://a.b)"),
                    5 => type_marked(&mut session, "*f* **g**"),
                    6 => {
                        session.delete_word_back();
                    }
                    7 => {
                        session.split();
                    }
                    8 => {
                        let block = session.document().blocks
                            [next(blocks.min(session.document().blocks.len()))]
                        .clone();
                        if !block.text.is_empty() {
                            let end = block.text.char_indices().count().min(3);
                            let end = block
                                .text
                                .char_indices()
                                .nth(end)
                                .map_or(block.text.len(), |(at, _)| at);
                            if end > 0 {
                                session.set_link(block.id, 0, end, "b.kame#Z".to_string());
                            }
                        }
                    }
                    9 => {
                        for _ in 0..=next(6) {
                            session.undo();
                        }
                    }
                    _ => {
                        for _ in 0..=next(6) {
                            session.redo();
                        }
                    }
                }
                assert_spans_fit(&session);
                edits += 1;
            }
            assert!(edits > 0);
            while session.undo() {
                assert_spans_fit(&session);
            }
            assert_eq!(session.document().blocks.len(), 1, "seed {seed}");
            assert_eq!(session.document().blocks[0].text, start, "seed {seed}");
            assert!(session.document().blocks[0].spans.is_empty(), "seed {seed}");
        }
    }

    #[test]
    fn undoing_a_delete_inside_a_link_then_the_link_restores_each_step() {
        let mut session = Session::anonymous().unwrap();
        session.insert("one two three");
        let block_id = session.document().blocks[0].id;
        session.set_link(block_id, 4, 7, "b.kame".into());
        let linked = session.document().blocks[0].spans.clone();
        // The whole of the linked word, and then only part of it.
        for (start, end) in [(4, 7), (5, 6), (2, 5), (6, 9)] {
            session.selection = Some(Selection {
                start: Cursor {
                    block_id,
                    offset: start,
                },
                end: Cursor {
                    block_id,
                    offset: end,
                },
            });
            session.delete_word_back();
            session.undo();
            assert_eq!(session.document().blocks[0].text, "one two three");
            assert_eq!(session.document().blocks[0].spans, linked, "{start}..{end}");
        }
        session.undo();
        assert!(session.document().blocks[0].spans.is_empty());
    }

    #[test]
    fn undoing_a_delete_of_bold_text_restores_the_bold() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "**word**");
        let spans = session.document().blocks[0].spans.clone();
        assert_eq!(spans.len(), 1);
        session.delete_word_back();
        assert!(session.document().blocks[0].spans.is_empty());
        session.undo();
        assert_eq!(session.document().blocks[0].spans, spans);
    }

    #[test]
    fn a_delete_written_by_an_older_version_still_reads() {
        let id = uuid::Uuid::new_v4();
        let json = format!(
            r#"{{"DeleteText":{{"block_id":"{id}","start":0,"end":1,"text":"a","removed":[]}}}}"#
        );
        let op: Op = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            op,
            Op::DeleteText {
                old_spans: None,
                ..
            }
        ));
    }

    fn on_an_image() -> Session {
        let path = temp_db_path();
        let mut session = open(&path);
        assert!(session.insert_image("plate.png".to_string()));
        let image = session
            .document()
            .blocks
            .iter()
            .find(|block| matches!(block.kind, BlockKind::Image { .. }))
            .unwrap()
            .id;
        session.set_caret(Cursor {
            block_id: image,
            offset: 0,
        });
        session
    }

    fn kinds_and_words(session: &Session) -> Vec<(bool, String)> {
        session
            .document()
            .blocks
            .iter()
            .map(|block| {
                (
                    matches!(block.kind, BlockKind::Image { .. }),
                    block.text.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn writing_on_an_image_starts_a_paragraph_after_it() {
        let mut session = on_an_image();
        let images = |session: &Session| {
            kinds_and_words(session)
                .iter()
                .filter(|(image, _)| *image)
                .count()
        };
        assert!(session.insert("typed"));
        assert_eq!(images(&session), 1);
        let blocks = kinds_and_words(&session);
        let at = blocks.iter().position(|(image, _)| *image).unwrap();
        assert_eq!(blocks[at], (true, String::new()), "the image holds no text");
        assert_eq!(blocks[at + 1], (false, "typed".to_string()));
        assert_eq!(session.caret.offset, "typed".len());
    }

    #[test]
    fn pressing_enter_on_an_image_does_not_make_another() {
        let mut session = on_an_image();
        let before = kinds_and_words(&session)
            .iter()
            .filter(|(image, _)| *image)
            .count();
        assert!(session.split());
        let after = kinds_and_words(&session)
            .iter()
            .filter(|(image, _)| *image)
            .count();
        assert_eq!(after, before);
    }

    /// A document saved by a build that knows a field this one doesn't. Nothing here may write
    /// over it, or the field is gone.
    fn from_a_newer_build() -> (PathBuf, String) {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("words");
        drop(session);
        let connection = rusqlite::Connection::open(&path).unwrap();
        let head: String = connection
            .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let newer = head.replacen("\"spans\":[]", "\"spans\":[],\"future\":1", 1);
        assert_ne!(newer, head);
        connection
            .execute("UPDATE documents SET json = ?1 WHERE id = 1", [&newer])
            .unwrap();
        (path, newer)
    }

    fn head_of(path: &Path) -> String {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn a_document_from_a_newer_build_opens_read_only_and_says_so() {
        let (path, _) = from_a_newer_build();
        let session = open(&path);
        assert!(session.newer);
        assert!(session.save_failed, "edits are refused");
        let said = session.last_error.clone().unwrap_or_default();
        assert!(said.contains("newer version"), "{said:?}");
        assert!(said.contains("read-only"), "{said:?}");
        assert!(!session.would_lose_work(), "nothing unsaved is at stake");
    }

    #[test]
    fn nothing_writes_over_a_document_from_a_newer_build() {
        let (path, written) = from_a_newer_build();
        let mut session = open(&path);
        assert!(!session.insert("more"));
        assert!(!session.undo());
        session.retry_or_dismiss();
        assert!(session.newer, "still read-only");
        assert!(session.last_error.is_some(), "and still saying so");
        session.set_metadata(Metadata {
            title: "Changed".to_string(),
            ..Metadata::default()
        });
        let snapshot = session.snapshot(Some("kept")).unwrap();
        assert!(!session.restore(snapshot));
        let elsewhere = temp_db_path();
        assert!(session.save_as(&elsewhere).is_err());
        assert!(!elsewhere.exists());
        drop(session);
        assert_eq!(head_of(&path), written);
        let reopened = open(&path);
        assert_ne!(reopened.metadata.title, "Changed");
    }

    #[test]
    fn a_step_of_history_from_a_newer_build_is_kept_and_not_cleared() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("words");
        drop(session);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO ops (json) VALUES ('{\"Future\":{\"a\":1}}')",
                [],
            )
            .unwrap();
        drop(connection);

        let session = open(&path);
        assert!(session.newer);
        assert_eq!(session.document().blocks[0].text, "words");
        drop(session);
        let rows: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM ops", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 2, "the history is all still there");
    }

    #[test]
    fn a_document_from_an_older_build_is_not_taken_for_a_newer_one() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("words");
        drop(session);
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE metadata SET json = '{\"title\":\"T\"}' WHERE id = 1",
                [],
            )
            .unwrap();
        drop(connection);
        assert!(!open(&path).newer);
    }

    fn linked(words: &str, from: usize, to: usize, url: &str) -> Session {
        let path = temp_db_path();
        let mut session = open(&path);
        assert!(session.insert(words));
        let block = session.document().blocks[0].id;
        assert!(session.set_link(block, from, to, url.to_string()));
        session
    }

    fn select_range(session: &mut Session, block: BlockId, from: usize, to: usize) {
        session.set_caret(Cursor {
            block_id: block,
            offset: from,
        });
        session.select_to(Cursor {
            block_id: block,
            offset: to,
        });
    }

    fn links_of(session: &Session, index: usize) -> Vec<(String, String)> {
        let block = &session.document().blocks[index];
        block
            .spans
            .iter()
            .filter_map(|span| match &span.style {
                SpanStyle::Link { url } | SpanStyle::DocumentLink { target: url } => {
                    Some((block.text[span.start..span.end].to_string(), url.clone()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn cutting_and_pasting_a_link_keeps_it() {
        let mut session = linked("See the paper now.", 8, 13, "https://p.example/");
        let block = session.document().blocks[0].id;
        select_range(&mut session, block, 8, 13);
        let clipped = session.clipped().unwrap();
        assert_eq!(clipped.text, "paper");
        assert!(session.backspace());
        assert_eq!(session.document().blocks[0].text, "See the  now.");
        assert!(session.paste_clipped("paper", Some(&clipped)));
        assert_eq!(session.document().blocks[0].text, "See the paper now.");
        assert_eq!(
            links_of(&session, 0),
            [("paper".to_string(), "https://p.example/".to_string())]
        );
        assert_eq!(session.caret.offset, 13);
    }

    #[test]
    fn pasting_a_link_is_one_undo_step() {
        let mut session = linked("A link", 2, 6, "https://l.example/");
        let block = session.document().blocks[0].id;
        select_range(&mut session, block, 2, 6);
        let clipped = session.clipped().unwrap();
        session.set_caret(Cursor {
            block_id: block,
            offset: 0,
        });
        let before = session.document().clone();
        let (depth, _) = session.history();
        assert!(session.paste_clipped("link", Some(&clipped)));
        assert_eq!(session.history().0, depth + 1);
        assert!(session.undo());
        assert_eq!(*session.document(), before);
    }

    #[test]
    fn what_was_copied_keeps_only_the_part_of_a_link_it_covers() {
        let mut session = linked("before words after", 7, 12, "https://w.example/");
        let block = session.document().blocks[0].id;
        select_range(&mut session, block, 3, 9);
        let clipped = session.clipped().unwrap();
        assert_eq!(clipped.text, "ore wo");
        session.set_caret(Cursor {
            block_id: block,
            offset: 18,
        });
        assert!(session.paste_clipped("ore wo", Some(&clipped)));
        assert!(
            links_of(&session, 0).contains(&("wo".to_string(), "https://w.example/".to_string()))
        );
    }

    /// The clipboard is the system's, so something copied elsewhere since is only text.
    #[test]
    fn text_other_than_what_was_copied_pastes_plain() {
        let mut session = linked("A link", 2, 6, "https://l.example/");
        let block = session.document().blocks[0].id;
        select_range(&mut session, block, 2, 6);
        let clipped = session.clipped().unwrap();
        session.set_caret(Cursor {
            block_id: block,
            offset: 0,
        });
        assert!(session.paste_clipped("other", Some(&clipped)));
        assert_eq!(links_of(&session, 0).len(), 1, "only the original link");
        assert_eq!(session.document().blocks[0].text, "otherA link");
    }

    #[test]
    fn pasting_keeps_every_kind_of_formatting_across_blocks() {
        let mut session = linked("one two", 4, 7, "https://t.example/");
        assert!(session.split());
        assert!(session.insert("three four"));
        let (first, second) = (
            session.document().blocks[0].id,
            session.document().blocks[1].id,
        );
        let bold = session.editor.set_spans_of(
            second,
            vec![Span {
                start: 0,
                end: 5,
                style: SpanStyle::Bold,
            }],
        );
        session.commit(bold);
        session.set_caret(Cursor {
            block_id: first,
            offset: 4,
        });
        session.select_to(Cursor {
            block_id: second,
            offset: 5,
        });
        let clipped = session.clipped().unwrap();
        assert_eq!(clipped.text, "two\nthree");

        session.set_caret(Cursor {
            block_id: second,
            offset: "three four".len(),
        });
        assert!(session.split());
        assert!(session.paste_clipped("two\nthree", Some(&clipped)));
        let blocks = &session.document().blocks;
        assert_eq!(blocks[2].text, "two");
        assert_eq!(blocks[3].text, "three");
        assert_eq!(
            links_of(&session, 2),
            [("two".to_string(), "https://t.example/".to_string())]
        );
        assert_eq!(session.document().blocks[3].spans[0].style, SpanStyle::Bold);
        assert_eq!(session.document().blocks[3].spans[0].end, 5);
    }

    #[test]
    fn code_takes_a_pasted_links_words_and_not_the_link() {
        let mut session = linked("A link", 2, 6, "https://l.example/");
        let block = session.document().blocks[0].id;
        select_range(&mut session, block, 2, 6);
        let clipped = session.clipped().unwrap();
        assert!(session.split());
        let code = session.valid_caret().block_id;
        assert!(session.set_block_kind(code, BlockKind::Code { language: None }));
        assert!(session.paste_clipped("link", Some(&clipped)));
        let block = session
            .document()
            .blocks
            .iter()
            .find(|block| block.id == code)
            .unwrap();
        assert!(block.text.contains("link"));
        assert!(block.spans.is_empty());
    }

    /// Text inserted inside a link is already part of it, and two links can't share words.
    #[test]
    fn a_link_pasted_inside_a_link_joins_it() {
        let mut session = linked("A link", 2, 6, "https://outer.example/");
        let mut source = linked("other", 0, 5, "https://inner.example/");
        let from = source.document().blocks[0].id;
        select_range(&mut source, from, 0, 5);
        let clipped = source.clipped().unwrap();
        session.set_caret(Cursor {
            block_id: session.document().blocks[0].id,
            offset: 4,
        });
        assert!(session.paste_clipped("other", Some(&clipped)));
        assert_eq!(
            links_of(&session, 0),
            [(
                "liothernk".to_string(),
                "https://outer.example/".to_string()
            )]
        );
    }

    /// Moved far from where it was copied: another document, in another folder.
    fn moved_to_another_folder(target: &str) -> String {
        let root = crate::test_temp_dir().join(format!("moved-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::create_dir_all(root.join("far/away")).unwrap();
        let mut source = open(&root.join("notes/a.kame"));
        assert!(source.insert("A link"));
        let block = source.document().blocks[0].id;
        assert!(source.set_link(block, 2, 6, target.to_string()));
        select_range(&mut source, block, 2, 6);
        let clipped = source.clipped().unwrap();

        let mut there = open(&root.join("far/away/c.kame"));
        assert!(there.paste_clipped("link", Some(&clipped)));
        links_of(&there, 0)[0].1.clone()
    }

    #[test]
    fn a_relative_link_still_leads_to_the_same_place_from_another_folder() {
        assert_eq!(moved_to_another_folder("b.kame"), "../../notes/b.kame");
        assert_eq!(
            moved_to_another_folder("b.kame#Intro"),
            "../../notes/b.kame#Intro"
        );
        assert_eq!(
            moved_to_another_folder("sub/b.kame"),
            "../../notes/sub/b.kame"
        );
        assert_eq!(moved_to_another_folder("../top.kame"), "../../top.kame");
        assert_eq!(moved_to_another_folder("../far/away/c.kame"), "c.kame");
        assert_eq!(
            moved_to_another_folder("../static/p.pdf"),
            "../../static/p.pdf"
        );
    }

    /// `#Intro` means a heading in this document, so elsewhere it has to name the document.
    #[test]
    fn a_link_to_a_heading_here_names_its_document_from_elsewhere() {
        assert_eq!(
            moved_to_another_folder("#Intro"),
            "../../notes/a.kame#Intro"
        );
    }

    #[test]
    fn links_that_dont_depend_on_where_they_are_stay_as_they_are() {
        for target in [
            "https://example.com/a.kame",
            "mailto:k@example.com",
            "/srv/whole/path.kame",
            "~/writing/a.kame",
            "//host/x",
        ] {
            assert_eq!(moved_to_another_folder(target), target);
        }
    }

    #[test]
    fn a_relative_link_is_untouched_within_a_document_or_a_folder() {
        let root = crate::test_temp_dir().join(format!("moved-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut source = open(&root.join("a.kame"));
        assert!(source.insert("One link and #here"));
        let block = source.document().blocks[0].id;
        assert!(source.set_link(block, 4, 8, "b.kame#Intro".to_string()));
        assert!(source.set_link(block, 13, 18, "#Intro".to_string()));
        select_range(&mut source, block, 4, 18);
        let clipped = source.clipped().unwrap();
        source.set_caret(Cursor {
            block_id: block,
            offset: 0,
        });
        assert!(source.paste_clipped(&clipped.text, Some(&clipped)));
        let urls: Vec<String> = links_of(&source, 0)
            .into_iter()
            .map(|(_, url)| url)
            .collect();
        assert!(
            urls.iter().filter(|url| *url == "#Intro").count() == 2,
            "{urls:?}"
        );

        let mut neighbour = open(&root.join("c.kame"));
        assert!(neighbour.paste_clipped(&clipped.text, Some(&clipped)));
        let urls: Vec<String> = links_of(&neighbour, 0)
            .into_iter()
            .map(|(_, url)| url)
            .collect();
        assert_eq!(urls[0], "b.kame#Intro", "same folder, same way there");
        assert_eq!(urls[1], "a.kame#Intro", "but no longer the same document");
    }

    /// With no file, there's no folder to say the way from.
    #[test]
    fn a_document_with_no_file_keeps_a_links_target() {
        let mut source = Session::anonymous().unwrap();
        assert!(source.insert("A link"));
        let block = source.document().blocks[0].id;
        assert!(source.set_link(block, 2, 6, "b.kame".to_string()));
        select_range(&mut source, block, 2, 6);
        let clipped = source.clipped().unwrap();
        let mut other = Session::anonymous().unwrap();
        assert!(other.paste_clipped("link", Some(&clipped)));
        assert_eq!(links_of(&other, 0)[0].1, "b.kame");
    }

    #[test]
    fn nothing_selected_means_nothing_clipped() {
        let session = linked("A link", 2, 6, "https://l.example/");
        assert_eq!(session.clipped(), None);
    }

    #[test]
    fn editing_a_link_changes_its_words_and_address_as_one_step() {
        let mut session = linked("See the paper now.", 8, 13, "https://p.example/");
        let block = session.document().blocks[0].id;
        let (depth, _) = session.history();
        assert!(session.edit_link(block, 8, 13, "study", "https://s.example/"));
        assert_eq!(session.document().blocks[0].text, "See the study now.");
        assert_eq!(
            links_of(&session, 0),
            [("study".to_string(), "https://s.example/".to_string())]
        );
        assert_eq!(session.history().0, depth + 1);
        assert_eq!(session.caret.offset, 13);
        assert!(session.undo());
        assert_eq!(session.document().blocks[0].text, "See the paper now.");
        assert_eq!(
            links_of(&session, 0),
            [("paper".to_string(), "https://p.example/".to_string())]
        );
    }

    #[test]
    fn a_link_with_no_words_shows_its_address() {
        let mut session = linked("See the paper now.", 8, 13, "https://p.example/");
        let block = session.document().blocks[0].id;
        assert!(session.edit_link(block, 8, 13, "", "https://p.example/"));
        assert_eq!(
            session.document().blocks[0].text,
            "See the https://p.example/ now."
        );
        assert_eq!(
            links_of(&session, 0),
            [(
                "https://p.example/".to_string(),
                "https://p.example/".to_string()
            )]
        );
    }

    #[test]
    fn an_empty_address_takes_the_link_away_and_keeps_words() {
        let mut session = linked("See the paper now.", 8, 13, "https://p.example/");
        let block = session.document().blocks[0].id;
        assert!(session.edit_link(block, 8, 13, "study", ""));
        assert_eq!(session.document().blocks[0].text, "See the study now.");
        assert!(links_of(&session, 0).is_empty());

        // With both empty there's nothing to show instead, so the words stay.
        let mut session = linked("See the paper now.", 8, 13, "https://p.example/");
        assert!(session.edit_link(block_of(&session), 8, 13, "", ""));
        assert_eq!(session.document().blocks[0].text, "See the paper now.");
        assert!(links_of(&session, 0).is_empty());
    }

    fn block_of(session: &Session) -> BlockId {
        session.document().blocks[0].id
    }

    #[test]
    fn formatting_around_a_link_survives_editing_its_words() {
        let mut session = linked("See the paper now.", 8, 13, "https://p.example/");
        let block = block_of(&session);
        let spans = vec![
            Span {
                start: 8,
                end: 13,
                style: SpanStyle::Link {
                    url: "https://p.example/".to_string(),
                },
            },
            Span {
                start: 8,
                end: 13,
                style: SpanStyle::Bold,
            },
            Span {
                start: 4,
                end: 18,
                style: SpanStyle::Italic,
            },
            Span {
                start: 14,
                end: 17,
                style: SpanStyle::Code,
            },
        ];
        let set = session.editor.set_spans_of(block, spans);
        session.commit(set);
        assert!(session.edit_link(block, 8, 13, "longer words", "https://p.example/"));
        let document = session.document();
        let text = &document.blocks[0].text;
        let on = |style: SpanStyle| {
            document.blocks[0]
                .spans
                .iter()
                .find(|span| span.style == style)
                .map(|span| text[span.start..span.end].to_string())
        };
        assert_eq!(on(SpanStyle::Bold).as_deref(), Some("longer words"));
        assert_eq!(
            on(SpanStyle::Italic).as_deref(),
            Some("the longer words now.")
        );
        assert_eq!(on(SpanStyle::Code).as_deref(), Some("now"));
    }

    #[test]
    fn editing_part_of_a_links_words_keeps_a_note_on_the_rest() {
        let mut session = linked("See the paper now.", 8, 13, "https://p.example/");
        let block = block_of(&session);
        let note = session.editor.add_annotation(
            AnnotationKind::Note,
            Anchor::range(block, 8, 11),
            "kept",
        );
        session.commit(note);
        assert!(session.edit_link(block, 8, 13, "paper!", "https://p.example/"));
        assert_eq!(session.document().annotations.len(), 1);
        assert_eq!(session.document().blocks[0].text, "See the paper! now.");
        assert_eq!(
            session.document().annotations[0].anchor,
            Anchor::range(block, 8, 11)
        );
    }

    #[test]
    fn editing_a_links_address_alone_is_a_small_step() {
        let mut session = linked("A link", 2, 6, "https://l.example/");
        let block = block_of(&session);
        assert!(session.edit_link(block, 2, 6, "link", "https://other.example/"));
        assert!(matches!(
            session.editor.applied_ops().last(),
            Some(Op::SetSpans { .. })
        ));
    }

    #[test]
    fn multibyte_words_in_a_link_are_counted_in_bytes() {
        let mut session = linked("Voilà été là", 7, 12, "https://e.example/");
        assert_eq!(links_of(&session, 0)[0].0, "été");
        let block = block_of(&session);
        assert!(session.edit_link(block, 7, 12, "épée", "https://e.example/"));
        assert_eq!(session.document().blocks[0].text, "Voilà épée là");
        assert_eq!(links_of(&session, 0)[0].0, "épée");
    }

    #[test]
    fn the_link_under_a_position_is_found_whatever_kind_it_is() {
        let session = linked(
            "A link https://bare.example/ end",
            2,
            6,
            "https://l.example/",
        );
        let block = block_of(&session);
        let at = |offset| Cursor {
            block_id: block,
            offset,
        };
        let found = session.link_under(at(3)).unwrap();
        assert_eq!((found.start, found.end), (2, 6));
        assert_eq!(found.url, "https://l.example/");
        assert!(session.link_under(at(6)).is_none(), "just past its end");
        assert!(session.link_under(at(0)).is_none());
        let bare = session.link_under(at(10)).unwrap();
        assert_eq!(bare.url, "https://bare.example/");
        assert_eq!((bare.start, bare.end), (7, 28));

        let mut session = linked("See notes", 4, 9, "notes/a.kame");
        let block = block_of(&session);
        let found = session
            .link_under(Cursor {
                block_id: block,
                offset: 5,
            })
            .unwrap();
        assert_eq!(found.url, "notes/a.kame");
        assert!(session.set_link(block, 4, 9, String::new()));
    }

    #[test]
    fn a_refused_undo_says_so_and_keeps_the_document() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("words");
        let block_id = session.document().blocks[0].id;
        let text_before = session.document().blocks[0].text.clone();
        let doc = session.document().clone();
        let stale = Op::InsertText {
            block_id,
            offset: 0,
            text: "elsewhere".to_string(),
        };
        session.editor = Editor::with_cursor(doc, vec![stale], 1);
        assert!(!session.undo());
        assert_eq!(session.document().blocks[0].text, text_before);
        let said = session.last_error.clone().unwrap_or_default();
        assert!(said.contains("undo"), "{said:?}");
        assert!(said.contains("napshot"), "{said:?}");
        assert!(!session.save_failed, "the document can still be edited");
    }

    #[test]
    fn a_note_cut_short_is_put_back_by_an_undo_after_reopening() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.insert("0123456789");
        let block = session.document().blocks[0].id;
        let at = |offset| Cursor {
            block_id: block,
            offset,
        };
        let op =
            session
                .editor
                .add_annotation(AnnotationKind::Note, Anchor::range(block, 2, 8), "n");
        session.commit(op);
        let cut = session.editor.delete_selection(&Selection {
            start: at(5),
            end: at(10),
        });
        session.commit(cut);
        assert_eq!(
            session.document().annotations[0].anchor,
            Anchor::range(block, 2, 5)
        );
        drop(session);

        let mut reopened = open(&path);
        assert!(reopened.undo());
        assert_eq!(
            reopened.document().annotations[0].anchor,
            Anchor::range(block, 2, 8)
        );
    }

    #[test]
    fn linking_a_document_at_the_caret_inserts_its_title_as_a_link() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "see  and *more*");
        let block_id = session.document().blocks[0].id;
        session.set_caret(Cursor {
            block_id,
            offset: 4,
        });
        let before = session.document().blocks[0].clone();
        session
            .link_to_document("~/a.kame".to_string(), "The Essay")
            .unwrap();
        let block = &session.document().blocks[0];
        assert_eq!(block.text, "see The Essay and more");
        assert_eq!(
            document_link_spans(&session),
            vec![(4, 13, "~/a.kame".to_string())]
        );
        assert!(block
            .spans
            .iter()
            .any(|span| span.style == SpanStyle::Italic && span.start == 18 && span.end == 22));
        assert_eq!(session.caret.offset, 13);
        session.undo();
        assert_eq!(
            session.document().blocks[0],
            before,
            "one undo takes it back"
        );
    }

    #[test]
    fn linking_a_document_over_a_selection_keeps_the_selected_words() {
        let mut session = Session::anonymous().unwrap();
        session.insert("the old essay");
        let block_id = session.document().blocks[0].id;
        session.selection = Some(Selection {
            start: Cursor {
                block_id,
                offset: 4,
            },
            end: Cursor {
                block_id,
                offset: 7,
            },
        });
        session
            .link_to_document("~/a.md".to_string(), "Ignored")
            .unwrap();
        assert_eq!(session.document().blocks[0].text, "the old essay");
        assert_eq!(
            document_link_spans(&session),
            vec![(4, 7, "~/a.md".to_string())]
        );
    }

    #[test]
    fn linking_a_document_with_the_caret_in_a_link_points_that_link_elsewhere() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "[[old|words]]");
        let block_id = session.document().blocks[0].id;
        session.set_caret(Cursor {
            block_id,
            offset: 2,
        });
        session
            .link_to_document("~/new.kame".to_string(), "Ignored")
            .unwrap();
        assert_eq!(session.document().blocks[0].text, "words");
        assert_eq!(
            document_link_spans(&session),
            vec![(0, 5, "~/new.kame".to_string())]
        );
    }

    #[test]
    fn a_document_link_cant_go_where_styles_dont() {
        let mut code = code_session();
        assert!(code.link_to_document("~/a.kame".to_string(), "T").is_err());
        assert_eq!(code.document().blocks[0].text, "");
        let mut rule = Session::anonymous().unwrap();
        typed(&mut rule, "---");
        rule.split();
        let block_id = rule.document().blocks[0].id;
        rule.set_caret(Cursor {
            block_id,
            offset: 0,
        });
        assert!(rule.link_to_document("~/a.kame".to_string(), "T").is_err());
    }

    #[test]
    fn a_selection_across_blocks_cant_be_linked() {
        let mut session = Session::anonymous().unwrap();
        session.insert("one");
        session.split();
        session.insert("two");
        let (first, second) = (
            session.document().blocks[0].id,
            session.document().blocks[1].id,
        );
        session.selection = Some(Selection {
            start: Cursor {
                block_id: first,
                offset: 1,
            },
            end: Cursor {
                block_id: second,
                offset: 1,
            },
        });
        assert!(session
            .link_to_document("~/a.kame".to_string(), "T")
            .is_err());
        assert_eq!(session.document().blocks.len(), 2);
    }

    fn select(session: &mut Session, start: (usize, usize), end: (usize, usize)) {
        let id = |index: usize| session.document().blocks[index].id;
        let selection = Selection {
            start: Cursor {
                block_id: id(start.0),
                offset: start.1,
            },
            end: Cursor {
                block_id: id(end.0),
                offset: end.1,
            },
        };
        session.selection = Some(selection);
    }

    #[test]
    fn nothing_selected_has_nothing_to_copy() {
        let mut session = Session::anonymous().unwrap();
        session.insert("words");
        assert_eq!(session.selected_text(), None);
        select(&mut session, (0, 2), (0, 2));
        assert_eq!(session.selected_text(), None);
    }

    #[test]
    fn a_selection_in_one_block_copies_its_bytes() {
        let mut session = Session::anonymous().unwrap();
        session.insert("une été ici");
        select(&mut session, (0, 4), (0, "une été".len()));
        assert_eq!(session.selected_text(), Some("été".to_string()));
    }

    #[test]
    fn a_backwards_selection_copies_the_same_text() {
        let mut session = Session::anonymous().unwrap();
        session.insert("words");
        select(&mut session, (0, 4), (0, 1));
        assert_eq!(session.selected_text(), Some("ord".to_string()));
    }

    #[test]
    fn a_selection_across_blocks_copies_a_line_for_each() {
        let mut session = Session::anonymous().unwrap();
        session.insert("first");
        session.split();
        session.insert("second");
        session.split();
        session.insert("third");
        select(&mut session, (0, 2), (2, 2));
        assert_eq!(session.selected_text(), Some("rst\nsecond\nth".to_string()));
    }

    #[test]
    fn what_is_copied_across_blocks_pastes_back_as_blocks() {
        let mut session = Session::anonymous().unwrap();
        session.insert("first");
        session.split();
        session.insert("second");
        select(&mut session, (0, 0), (1, 6));
        let copied = session.selected_text().unwrap();
        let mut other = Session::anonymous().unwrap();
        other.paste(&copied);
        assert_eq!(other.document().blocks.len(), 2);
        assert_eq!(other.document().blocks[1].text, "second");
    }

    #[test]
    fn a_selection_in_code_copies_its_line_breaks() {
        let mut session = code_session();
        session.paste("a\n\tb");
        select(&mut session, (0, 0), (0, 4));
        assert_eq!(session.selected_text(), Some("a\n\tb".to_string()));
    }

    fn code_spans(session: &Session) -> Vec<(usize, usize)> {
        session.document().blocks[0]
            .spans
            .iter()
            .filter(|span| span.style == SpanStyle::Code)
            .map(|span| (span.start, span.end))
            .collect()
    }

    #[test]
    fn typed_backticks_make_inline_code() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "run `make`");
        assert_eq!(session.document().blocks[0].text, "run make");
        assert_eq!(code_spans(&session), vec![(4, 8)]);
        type_marked(&mut session, " now");
        assert_eq!(session.document().blocks[0].text, "run make now");
        assert_eq!(
            code_spans(&session),
            vec![(4, 8)],
            "what follows isn't code"
        );
        // " " and "now", and then the code.
        session.undo();
        session.undo();
        session.undo();
        assert_eq!(session.document().blocks[0].text, "run `make`");
        assert!(code_spans(&session).is_empty());
    }

    #[test]
    fn markup_inside_open_backticks_stays_literal() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "`a_b_c *d* [e](f) [[g]]`");
        assert_eq!(session.document().blocks[0].text, "a_b_c *d* [e](f) [[g]]");
        assert_eq!(session.document().blocks[0].spans.len(), 1);
    }

    #[test]
    fn typing_inside_inline_code_stays_literal() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "`abcd`");
        session.caret.offset = 2;
        type_marked(&mut session, "*x*");
        assert_eq!(session.document().blocks[0].text, "ab*x*cd");
        assert_eq!(code_spans(&session), vec![(0, 7)]);
    }

    #[test]
    fn emphasis_around_inline_code_still_works() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "*use `it` now*");
        assert_eq!(session.document().blocks[0].text, "use it now");
        assert!(session.document().blocks[0]
            .spans
            .iter()
            .any(|span| span.style == SpanStyle::Italic));
    }

    #[test]
    fn empty_or_unfinished_backticks_stay_as_typed() {
        for typed_text in ["``", "`a", "a`", "``a`"] {
            let mut session = Session::anonymous().unwrap();
            type_marked(&mut session, typed_text);
            assert_eq!(session.document().blocks[0].text, typed_text);
            assert!(code_spans(&session).is_empty(), "{typed_text}");
        }
    }

    #[test]
    fn a_fence_still_starts_a_code_block() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "```rust");
        session.split();
        assert_eq!(
            session.document().blocks[0].kind,
            BlockKind::Code {
                language: Some("rust".to_string())
            }
        );
    }

    #[test]
    fn backticks_stay_literal_in_code_and_prose_blocks() {
        let mut code = code_session();
        type_marked(&mut code, "`a`");
        assert_eq!(code.document().blocks[0].text, "`a`");
        let mut prose = Session::anonymous().unwrap();
        typed(&mut prose, "| ");
        type_marked(&mut prose, "`a`");
        assert_eq!(prose.document().blocks[0].text, "`a`");
    }

    fn list_kind(session: &Session, index: usize) -> (bool, u8) {
        match session.document().blocks[index].kind {
            BlockKind::ListItem { ordered, depth } => (ordered, depth),
            ref other => panic!("not a list item: {other:?}"),
        }
    }

    fn three_items() -> Session {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "- one");
        session.split();
        typed(&mut session, "two");
        session.split();
        typed(&mut session, "three");
        session
    }

    #[test]
    fn tab_indents_a_list_item_and_shift_tab_brings_it_back() {
        let mut session = three_items();
        assert!(session.indent_list_item(true));
        assert_eq!(list_kind(&session, 2), (false, 1));
        assert!(session.indent_list_item(false));
        assert_eq!(list_kind(&session, 2), (false, 0));
        assert_eq!(session.caret.offset, "three".len());
    }

    #[test]
    fn indenting_is_one_undo_step() {
        let mut session = three_items();
        session.indent_list_item(true);
        session.undo();
        assert_eq!(list_kind(&session, 2), (false, 0));
    }

    #[test]
    fn an_item_can_only_go_one_level_below_the_one_before_it() {
        let mut session = three_items();
        assert!(session.indent_list_item(true));
        assert!(!session.indent_list_item(true), "that would skip a level");
        assert_eq!(list_kind(&session, 2), (false, 1));
    }

    #[test]
    fn the_first_item_of_a_list_cant_indent_or_outdent() {
        let mut session = three_items();
        let first = session.document().blocks[0].id;
        session.set_caret(Cursor {
            block_id: first,
            offset: 0,
        });
        assert!(!session.indent_list_item(true));
        assert!(!session.indent_list_item(false));
        assert_eq!(list_kind(&session, 0), (false, 0));
    }

    #[test]
    fn body_text_doesnt_indent() {
        let mut session = Session::anonymous().unwrap();
        session.insert("words");
        assert!(!session.indent_list_item(true));
    }

    #[test]
    fn enter_on_a_nested_item_makes_another_at_that_level() {
        let mut session = three_items();
        session.indent_list_item(true);
        session.split();
        assert_eq!(list_kind(&session, 3), (false, 1));
    }

    #[test]
    fn enter_on_an_empty_nested_item_comes_out_a_level_at_a_time() {
        let mut session = three_items();
        session.indent_list_item(true);
        session.split();
        session.split();
        assert_eq!(session.document().blocks.len(), 4);
        assert_eq!(list_kind(&session, 3), (false, 0));
        session.split();
        assert_eq!(session.document().blocks.len(), 4);
        assert_eq!(session.document().blocks[3].kind, BlockKind::Paragraph);
    }

    #[test]
    fn changing_a_nested_items_kind_keeps_its_level() {
        let mut session = three_items();
        session.indent_list_item(true);
        let id = session.caret.block_id;
        session.set_block_kind(
            id,
            BlockKind::ListItem {
                ordered: true,
                depth: 0,
            },
        );
        assert_eq!(list_kind(&session, 2), (true, 1));
    }

    fn depths(session: &Session) -> Vec<u8> {
        session
            .document()
            .blocks
            .iter()
            .map(|block| match block.kind {
                BlockKind::ListItem { depth, .. } => depth,
                _ => u8::MAX,
            })
            .collect()
    }

    fn five_items() -> Session {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "- one");
        for word in ["two", "three", "four", "five"] {
            session.split();
            typed(&mut session, word);
        }
        session
    }

    #[test]
    fn tab_moves_every_selected_item_as_one_undo_step() {
        let mut session = five_items();
        select(&mut session, (1, 1), (3, 1));
        assert!(session.indent_list_item(true));
        assert_eq!(depths(&session), vec![0, 1, 1, 1, 0]);
        assert!(
            session.selection.is_some(),
            "the selection stays for another Tab"
        );
        session.undo();
        assert_eq!(depths(&session), vec![0, 0, 0, 0, 0]);
    }

    #[test]
    fn a_backwards_selection_moves_the_same_items() {
        let mut session = five_items();
        select(&mut session, (3, 1), (1, 1));
        assert!(session.indent_list_item(true));
        assert_eq!(depths(&session), vec![0, 1, 1, 1, 0]);
    }

    #[test]
    fn shift_tab_moves_every_selected_item_that_can_come_out() {
        let mut session = five_items();
        select(&mut session, (1, 1), (3, 1));
        session.indent_list_item(true);
        select(&mut session, (0, 1), (2, 1));
        assert!(session.indent_list_item(false));
        assert_eq!(depths(&session), vec![0, 0, 0, 1, 0]);
    }

    #[test]
    fn selected_items_keep_their_shape_when_they_move() {
        let mut session = five_items();
        select(&mut session, (2, 1), (2, 2));
        session.indent_list_item(true);
        assert_eq!(depths(&session), vec![0, 0, 1, 0, 0]);
        select(&mut session, (1, 1), (3, 1));
        assert!(session.indent_list_item(true));
        assert_eq!(depths(&session), vec![0, 1, 2, 1, 0]);
    }

    #[test]
    fn a_selection_that_cant_all_move_doesnt_move_at_all() {
        let mut session = five_items();
        select(&mut session, (0, 1), (2, 1));
        assert!(
            !session.indent_list_item(true),
            "the first item has nowhere to go"
        );
        assert_eq!(depths(&session), vec![0, 0, 0, 0, 0]);
    }

    #[test]
    fn a_selection_ending_at_the_start_of_an_item_leaves_that_item_out() {
        let mut session = five_items();
        select(&mut session, (1, 0), (3, 0));
        assert!(session.indent_list_item(true));
        assert_eq!(depths(&session), vec![0, 1, 1, 0, 0]);
    }

    #[test]
    fn a_selection_within_one_item_moves_that_item() {
        let mut session = five_items();
        select(&mut session, (1, 0), (1, 2));
        assert!(session.indent_list_item(true));
        assert_eq!(depths(&session), vec![0, 1, 0, 0, 0]);
    }

    #[test]
    fn body_text_in_a_selection_stays_where_it_is() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "- one");
        session.split();
        typed(&mut session, "two");
        session.split();
        session.split();
        typed(&mut session, "body");
        select(&mut session, (1, 1), (2, 2));
        assert!(session.indent_list_item(true));
        assert_eq!(depths(&session), vec![0, 1, u8::MAX]);
    }

    #[test]
    fn outdenting_leaves_children_where_they_are() {
        let mut session = five_items();
        select(&mut session, (1, 1), (3, 1));
        session.indent_list_item(true);
        select(&mut session, (2, 1), (2, 1));
        session.selection = None;
        let parent = session.document().blocks[1].id;
        session.set_caret(Cursor {
            block_id: parent,
            offset: 0,
        });
        session.indent_list_item(false);
        assert_eq!(depths(&session), vec![0, 0, 1, 1, 0]);
    }

    #[test]
    fn a_date_that_wont_parse_gets_a_warning() {
        let mut metadata = Metadata::default();
        assert_eq!(date_warning(&metadata), None, "empty dates are fine");
        metadata.created = "2026-11-02".to_string();
        metadata.published = "2026-11-02 10:30".to_string();
        assert_eq!(date_warning(&metadata), None);
        metadata.published = "next week".to_string();
        assert!(date_warning(&metadata).unwrap().contains("next week"));
    }

    #[test]
    fn undo_takes_back_a_word_at_a_time() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "one two three");
        assert_eq!(session.history().0, 3);
        session.undo();
        assert_eq!(session.document().blocks[0].text, "one two ");
        session.undo();
        assert_eq!(session.document().blocks[0].text, "one ");
        session.redo();
        assert_eq!(session.document().blocks[0].text, "one two ");
    }

    #[test]
    fn typing_after_an_undo_starts_a_new_entry() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "one");
        session.undo();
        typed(&mut session, "x");
        assert_eq!(session.history(), (1, 0));
        assert_eq!(session.document().blocks[0].text, "x");
    }

    #[test]
    fn what_is_saved_is_a_word_at_a_time_too() {
        let path = temp_db_path();
        let mut session = open(&path);
        typed(&mut session, "one two");
        drop(session);
        let mut reopened = open(&path);
        assert_eq!(reopened.history().0, 2);
        assert_eq!(reopened.document().blocks[0].text, "one two");
        reopened.undo();
        assert_eq!(reopened.document().blocks[0].text, "one ");
    }

    #[test]
    fn a_keystroke_is_on_disk_at_once() {
        let path = temp_db_path();
        let mut session = open(&path);
        typed(&mut session, "wor");
        let db = crate::storage::Db::open_read_only(&path).unwrap();
        assert_eq!(db.load_document().unwrap().blocks[0].text, "wor");
        assert_eq!(db.load_ops().unwrap().len(), 1);
        drop(session);
    }

    #[test]
    fn backspacing_a_word_is_one_undo() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "one two");
        for _ in 0..3 {
            session.backspace();
        }
        assert_eq!(session.document().blocks[0].text, "one ");
        session.undo();
        assert_eq!(session.document().blocks[0].text, "one two");
    }

    #[test]
    fn typed_markup_still_undoes_on_its_own() {
        let mut session = Session::anonymous().unwrap();
        type_marked(&mut session, "*word*");
        assert_eq!(session.document().blocks[0].text, "word");
        session.undo();
        assert_eq!(session.document().blocks[0].text, "*word*");
    }

    #[test]
    fn enter_separates_words_into_their_own_entries() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "one");
        session.split();
        typed(&mut session, "two");
        session.undo();
        assert_eq!(session.document().blocks[1].text, "");
        session.undo();
        assert_eq!(session.document().blocks.len(), 1);
    }

    /// A file saved before typing coalesced: one log entry per keystroke.
    fn keystroke_file(words: &str) -> PathBuf {
        let path = temp_db_path();
        let mut doc = Document::new("Old");
        let block_id = doc.blocks[0].id;
        let mut db = Db::open(&path).unwrap();
        db.save_document(&doc).unwrap();
        for (offset, c) in words.char_indices() {
            doc.blocks[0].text.push(c);
            let op = Op::InsertText {
                block_id,
                offset,
                text: c.to_string(),
            };
            db.record_edit(&doc, &op).unwrap();
        }
        path
    }

    #[test]
    fn compacting_joins_old_keystrokes_and_keeps_the_text() {
        let path = keystroke_file("one two three");
        let mut session = open(&path);
        assert_eq!(session.history().0, 13);
        assert_eq!(session.compact_history(), Ok((13, 3)));
        assert_eq!(session.history().0, 3);
        assert_eq!(session.document().blocks[0].text, "one two three");
        session.undo();
        assert_eq!(session.document().blocks[0].text, "one two ");
        session.redo();
        assert_eq!(session.document().blocks[0].text, "one two three");
    }

    #[test]
    fn a_compacted_history_is_saved_and_undoes_all_the_way_back() {
        let path = keystroke_file("one two three");
        let mut session = open(&path);
        session.compact_history().unwrap();
        drop(session);
        let mut reopened = open(&path);
        assert_eq!(reopened.history().0, 3);
        assert_eq!(reopened.document().blocks[0].text, "one two three");
        while reopened.undo() {}
        assert_eq!(reopened.document().blocks[0].text, "");
        while reopened.redo() {}
        assert_eq!(reopened.document().blocks[0].text, "one two three");
    }

    #[test]
    fn compacting_shrinks_the_file() {
        let path = keystroke_file(&"lorem ipsum dolor sit amet ".repeat(200));
        let before = std::fs::metadata(&path).unwrap().len();
        let mut session = open(&path);
        session.compact_history().unwrap();
        drop(session);
        let after = std::fs::metadata(&path).unwrap().len();
        assert!(after * 2 < before, "{before} -> {after}");
    }

    #[test]
    fn compacting_keeps_what_redo_can_still_return_to() {
        let path = keystroke_file("one two");
        let mut session = open(&path);
        session.undo();
        session.undo();
        assert_eq!(session.document().blocks[0].text, "one t");
        assert_eq!(session.compact_history(), Ok((5, 2)));
        assert_eq!(session.history(), (2, 2));
        while session.redo() {}
        assert_eq!(session.document().blocks[0].text, "one two");
    }

    #[test]
    fn a_history_that_doesnt_agree_is_left_alone() {
        let path = keystroke_file("one two");
        let mut db = Db::open(&path).unwrap();
        let mut wrong = db.load_document().unwrap();
        wrong.blocks[0].text = "x".to_string();
        db.save_document(&wrong).unwrap();
        drop(db);
        let mut session = open(&path);
        let before = session.history();
        assert!(session.compact_history().is_err());
        assert_eq!(session.history(), before);
        assert_eq!(session.document().blocks[0].text, "x");
        drop(session);
        assert_eq!(Db::open(&path).unwrap().load_ops().unwrap().len(), 7);
    }

    #[test]
    fn compacting_a_compact_history_changes_nothing() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "one two");
        assert_eq!(session.compact_history(), Ok((2, 2)));
    }

    #[test]
    fn a_failed_save_stops_compacting() {
        let mut session = Session::anonymous().unwrap();
        typed(&mut session, "one");
        session.save_failed = true;
        assert!(session.compact_history().is_err());
    }

    /// Whatever editing got here, compacting must leave the same undo and redo journey.
    #[test]
    fn random_edits_compact_to_the_same_document() {
        for seed in 1..=8u64 {
            let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let mut next = move |limit: usize| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state % limit.max(1) as u64) as usize
            };
            let mut session = Session::anonymous().unwrap();
            for _ in 0..80 {
                let blocks = session.document().blocks.len();
                let block = session.document().blocks[next(blocks)].clone();
                let mut offset = next(block.text.len() + 1);
                while !block.text.is_char_boundary(offset) {
                    offset -= 1;
                }
                session.set_caret(Cursor {
                    block_id: block.id,
                    offset,
                });
                match next(5) {
                    0 => type_marked(&mut session, "été word "),
                    1 => type_marked(&mut session, "*f* [[a|b]] "),
                    2 => {
                        session.delete_word_back();
                    }
                    3 => {
                        session.split();
                    }
                    _ => {
                        session.undo();
                    }
                }
            }
            let head = session.document().clone();
            session.compact_history().unwrap();
            assert_eq!(session.document().blocks, head.blocks, "seed {seed}");
            // Only as far as it went: edits ending on an undo leave redo history beyond here.
            let depth = session.history().0;
            while session.undo() {}
            assert_eq!(session.document().blocks.len(), 1, "seed {seed}");
            assert_eq!(session.document().blocks[0].text, "", "seed {seed}");
            for _ in 0..depth {
                session.redo();
            }
            let texts = |doc: &Document| -> Vec<String> {
                doc.blocks.iter().map(|block| block.text.clone()).collect()
            };
            assert_eq!(texts(session.document()), texts(&head), "seed {seed}");
        }
    }

    #[test]
    fn a_new_file_takes_the_default_author_and_keeps_it() {
        let path = temp_db_path();
        let mut session = open(&path);
        session.apply_default_author("K. Isom");
        assert_eq!(session.metadata.author, "K. Isom");
        drop(session);
        assert_eq!(open(&path).metadata.author, "K. Isom");
    }

    #[test]
    fn an_existing_document_with_no_author_is_left_alone() {
        let path = temp_db_path();
        drop(open(&path));
        let mut session = open(&path);
        session.apply_default_author("K. Isom");
        assert_eq!(session.metadata.author, "");
        drop(session);
        assert_eq!(open(&path).metadata.author, "");
    }

    #[test]
    fn an_empty_document_takes_the_default_author() {
        let mut session = Session::anonymous().unwrap();
        session.apply_default_author("K. Isom");
        assert_eq!(session.metadata.author, "K. Isom");
    }

    #[test]
    fn a_new_documents_author_survives_its_first_save() {
        let mut session = Session::anonymous().unwrap();
        session.apply_default_author("K. Isom");
        session.insert("Words");
        let path = temp_db_path();
        session.save_as(&path).unwrap();
        drop(session);
        assert_eq!(open(&path).metadata.author, "K. Isom");
    }

    fn imported(source: &str) -> Session {
        let dir = scratch_dir("author");
        let path = dir.join("post.md");
        std::fs::write(&path, source).unwrap();
        Session::import(&path).unwrap().0
    }

    #[test]
    fn an_import_naming_its_author_keeps_them() {
        let mut session = imported("---\ntitle: T\nauthor: Someone Else\n---\n\nWords.\n");
        session.apply_default_author("K. Isom");
        assert_eq!(session.metadata.author, "Someone Else");
    }

    #[test]
    fn an_import_naming_no_author_takes_the_default() {
        let mut session = imported("Words.\n");
        session.apply_default_author("K. Isom");
        assert_eq!(session.metadata.author, "K. Isom");
    }

    #[test]
    fn the_default_is_only_applied_once() {
        let mut session = Session::anonymous().unwrap();
        session.apply_default_author("K. Isom");
        let mut metadata = session.metadata.clone();
        metadata.author = String::new();
        session.set_metadata(metadata);
        session.apply_default_author("K. Isom");
        assert_eq!(session.metadata.author, "");
    }

    fn selected_after_double_click(text: &str, offset: usize) -> Option<String> {
        let mut session = Session::anonymous().unwrap();
        session.insert(text);
        let block_id = session.document().blocks[0].id;
        session.select_word_at(Cursor { block_id, offset });
        session.selected_text()
    }

    #[test]
    fn a_double_click_selects_the_word_under_it() {
        assert_eq!(
            selected_after_double_click("one two three", 5).as_deref(),
            Some("two")
        );
        assert_eq!(
            selected_after_double_click("one two three", 4).as_deref(),
            Some("two")
        );
        assert_eq!(
            selected_after_double_click("one two three", 0).as_deref(),
            Some("one")
        );
    }

    #[test]
    fn a_double_click_counts_bytes_and_keeps_apostrophes() {
        assert_eq!(
            selected_after_double_click("un été ici", 5).as_deref(),
            Some("été")
        );
        assert_eq!(
            selected_after_double_click("don't go", 2).as_deref(),
            Some("don't")
        );
    }

    #[test]
    fn a_double_click_on_nothing_selects_nothing_and_places_the_caret() {
        let mut session = Session::anonymous().unwrap();
        session.insert("a  b");
        let block_id = session.document().blocks[0].id;
        session.select_word_at(Cursor {
            block_id,
            offset: 2,
        });
        assert_eq!(session.selected_text(), None);
        assert_eq!(session.caret.offset, 2);
    }

    #[test]
    fn a_double_click_replaces_an_earlier_selection_and_leaves_the_caret_at_its_end() {
        let mut session = Session::anonymous().unwrap();
        session.insert("one two three");
        let block_id = session.document().blocks[0].id;
        session.selection = Some(Selection {
            start: Cursor {
                block_id,
                offset: 0,
            },
            end: Cursor {
                block_id,
                offset: 3,
            },
        });
        session.select_word_at(Cursor {
            block_id,
            offset: 9,
        });
        assert_eq!(session.selected_text().as_deref(), Some("three"));
        assert_eq!(session.caret.offset, 13);
    }

    #[test]
    fn typing_replaces_a_double_clicked_word() {
        let mut session = Session::anonymous().unwrap();
        session.insert("one two three");
        let block_id = session.document().blocks[0].id;
        session.select_word_at(Cursor {
            block_id,
            offset: 5,
        });
        session.insert("2");
        assert_eq!(session.document().blocks[0].text, "one 2 three");
    }

    fn dragged_words(
        text: &[&str],
        anchor: (usize, usize),
        to: (usize, usize),
    ) -> (Option<String>, usize) {
        let mut session = Session::anonymous().unwrap();
        session.insert(text[0]);
        for line in &text[1..] {
            session.split();
            session.insert(line);
        }
        let id = |index: usize| session.document().blocks[index].id;
        let at = |(block, offset): (usize, usize)| Cursor {
            block_id: id(block),
            offset,
        };
        let word = session.word_range_at(at(anchor));
        let target = at(to);
        session.select_words_between(word, target);
        (session.selected_text(), session.caret.offset)
    }

    #[test]
    fn dragging_after_a_double_click_takes_whole_words_forwards() {
        let (selected, caret) = dragged_words(&["one two three four"], (0, 5), (0, 9));
        assert_eq!(selected.as_deref(), Some("two three"));
        assert_eq!(caret, "one two three".len());
    }

    #[test]
    fn dragging_after_a_double_click_takes_whole_words_backwards() {
        let (selected, caret) = dragged_words(&["one two three four"], (0, 9), (0, 1));
        assert_eq!(selected.as_deref(), Some("one two three"));
        assert_eq!(caret, 0, "the caret follows the pointer");
    }

    #[test]
    fn dragging_within_the_word_keeps_the_word() {
        let (selected, _) = dragged_words(&["one two three"], (0, 5), (0, 6));
        assert_eq!(selected.as_deref(), Some("two"));
    }

    #[test]
    fn dragging_by_words_crosses_blocks_both_ways() {
        let (selected, _) = dragged_words(&["one two", "three four"], (0, 5), (1, 1));
        assert_eq!(selected.as_deref(), Some("two\nthree"));
        let (selected, _) = dragged_words(&["one two", "three four"], (1, 1), (0, 5));
        assert_eq!(selected.as_deref(), Some("two\nthree"));
    }

    #[test]
    fn dragging_by_words_counts_bytes() {
        let (selected, _) = dragged_words(&["un été ici"], (0, 4), (0, "un été i".len()));
        assert_eq!(selected.as_deref(), Some("été ici"));
    }

    #[test]
    fn a_word_range_on_nothing_is_empty() {
        let mut session = Session::anonymous().unwrap();
        session.insert("a  b");
        let at = Cursor {
            block_id: session.document().blocks[0].id,
            offset: 2,
        };
        assert_eq!(session.word_range_at(at), (at, at));
    }

    #[test]
    fn dragging_by_words_with_nothing_between_selects_nothing() {
        let mut session = Session::anonymous().unwrap();
        session.insert("a  b");
        let at = Cursor {
            block_id: session.document().blocks[0].id,
            offset: 2,
        };
        let anchor = session.word_range_at(at);
        session.select_words_between(anchor, at);
        assert_eq!(session.selection, None);
        assert_eq!(session.caret, at);
    }

    /// A session on a new file with `text` in it, then closed, as another machine's copy.
    fn another_copy(path: &Path, text: &str) {
        let mut other = Session::open(path).unwrap();
        other.insert(text);
    }

    fn guard_dir() -> PathBuf {
        let dir = crate::test_temp_dir().join(format!("guard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn text_in(path: &Path) -> String {
        Session::open(path).unwrap().document().blocks[0]
            .text
            .clone()
    }

    #[test]
    fn a_file_left_alone_is_not_replaced() {
        let dir = guard_dir();
        let mut session = Session::open(&dir.join("a.kame")).unwrap();
        session.insert("mine");
        assert!(!session.check_file());
        assert!(!session.replaced);
        assert!(!session.save_failed);
    }

    #[test]
    fn a_document_with_no_file_is_never_replaced() {
        let mut session = Session::anonymous().unwrap();
        session.insert("mine");
        assert!(!session.check_file());
    }

    #[test]
    fn a_file_renamed_over_the_open_one_is_noticed_once() {
        let dir = guard_dir();
        let path = dir.join("a.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("mine");
        another_copy(&dir.join("b.kame"), "theirs");
        std::fs::rename(dir.join("b.kame"), &path).unwrap();
        assert!(session.check_file());
        assert!(session.replaced && session.save_failed);
        assert!(session.last_error.as_deref().unwrap().contains("a.kame"));
        assert!(!session.check_file(), "already noticed");
    }

    #[test]
    fn a_removed_file_counts_as_replaced() {
        let dir = guard_dir();
        let path = dir.join("a.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("mine");
        std::fs::remove_file(&path).unwrap();
        assert!(session.check_file());
        assert!(session.replaced);
    }

    #[test]
    fn an_edit_made_before_anyone_looked_is_refused_and_flagged() {
        let dir = guard_dir();
        let path = dir.join("a.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("mine");
        another_copy(&dir.join("b.kame"), "theirs");
        std::fs::rename(dir.join("b.kame"), &path).unwrap();
        session.insert(" more");
        assert!(session.replaced && session.save_failed);
        assert_eq!(text_in(&path), "theirs");
    }

    #[test]
    fn retrying_does_not_pretend_the_file_is_back() {
        let dir = guard_dir();
        let path = dir.join("a.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("mine");
        another_copy(&dir.join("b.kame"), "theirs");
        std::fs::rename(dir.join("b.kame"), &path).unwrap();
        session.check_file();
        assert!(!session.retry_or_dismiss());
        assert!(session.replaced && session.save_failed);
        session.clear_save_error();
        assert!(session.save_failed, "only a new file clears this");
    }

    #[test]
    fn saving_elsewhere_keeps_my_words_and_leaves_theirs_alone() {
        let dir = guard_dir();
        let path = dir.join("a.kame");
        let mine = dir.join("mine.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("mine");
        another_copy(&dir.join("b.kame"), "theirs");
        std::fs::rename(dir.join("b.kame"), &path).unwrap();
        session.check_file();
        session.save_as(&mine).unwrap();
        assert!(!session.replaced && !session.save_failed);
        assert!(session.insert_then_saved(" more"));
        assert!(!session.check_file(), "the new file is the one now open");
        drop(session);
        assert_eq!(text_in(&mine), "mine more");
        assert_eq!(text_in(&path), "theirs");
    }

    #[test]
    fn saving_over_the_replaced_path_is_refused() {
        let dir = guard_dir();
        let path = dir.join("a.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("mine");
        another_copy(&dir.join("b.kame"), "theirs");
        std::fs::rename(dir.join("b.kame"), &path).unwrap();
        session.check_file();
        assert!(session.save_as(&path).is_err());
        assert!(session.replaced);
    }

    /// SQLite leaves a moved database's `-wal` at its path when the connection closes, so
    /// waiting for that would pair this log with the new file on the next open, however that
    /// open came about: from here, or after a quit or a crash.
    #[test]
    fn noticing_takes_my_log_away_from_the_new_file() {
        let dir = guard_dir();
        let path = dir.join("a.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("mine");
        let wal = wal_path(&path);
        assert!(wal.exists(), "the test needs a live log");
        another_copy(&dir.join("b.kame"), "theirs");
        std::fs::rename(dir.join("b.kame"), &path).unwrap();
        session.check_file();
        assert!(!wal.exists(), "my log is not the new file's");
        assert_eq!(text_in(&path), "theirs", "while this session is still open");
        session.insert_then_saved(" more");
        assert_eq!(
            session.document().blocks[0].text,
            "mine",
            "still here to save"
        );
        drop(session);
        assert_eq!(text_in(&path), "theirs", "and after it closes");
    }

    /// A sync that also carries `-wal` files brings the log that goes with the new file.
    #[test]
    fn a_log_that_arrived_with_the_new_file_is_left_alone() {
        let dir = guard_dir();
        let path = dir.join("a.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("mine");
        another_copy(&dir.join("b.kame"), "theirs");
        std::fs::rename(dir.join("b.kame"), &path).unwrap();
        std::fs::write(dir.join("incoming"), b"their log").unwrap();
        std::fs::rename(dir.join("incoming"), wal_path(&path)).unwrap();
        session.check_file();
        assert_eq!(std::fs::read(wal_path(&path)).unwrap(), b"their log");
    }

    #[test]
    fn a_normal_close_takes_its_log_with_it() {
        let dir = guard_dir();
        let path = dir.join("a.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("mine");
        assert!(wal_path(&path).exists());
        drop(session);
        assert!(!wal_path(&path).exists());
    }

    #[test]
    fn a_log_that_arrived_survives_the_old_session_closing() {
        let dir = guard_dir();
        let path = dir.join("a.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("mine");
        another_copy(&dir.join("b.kame"), "theirs");
        std::fs::rename(dir.join("b.kame"), &path).unwrap();
        std::fs::write(dir.join("incoming"), b"their log").unwrap();
        std::fs::rename(dir.join("incoming"), wal_path(&path)).unwrap();
        session.check_file();
        drop(session);
        assert_eq!(std::fs::read(wal_path(&path)).unwrap(), b"their log");
    }

    /// The order "Open the one on disk" uses: the new session opens while the old one is still
    /// alive, and the old one closing must not take the new one's log with it.
    #[test]
    fn opening_the_new_file_survives_the_old_session_closing() {
        let dir = guard_dir();
        let path = dir.join("a.kame");
        let mut old = Session::open(&path).unwrap();
        old.insert("mine");
        another_copy(&dir.join("b.kame"), "theirs");
        std::fs::rename(dir.join("b.kame"), &path).unwrap();
        old.check_file();
        let mut new = Session::open(&path).unwrap();
        new.insert(" and more");
        assert!(wal_path(&path).exists(), "the new session's log");
        drop(old);
        assert!(wal_path(&path).exists(), "still the new session's log");
        new.insert(" again");
        assert!(!new.save_failed);
        // Copy without closing, as a crash would leave it.
        let crash = dir.join("crash");
        std::fs::create_dir_all(&crash).unwrap();
        let copied = crash.join("a.kame");
        std::fs::copy(&path, &copied).unwrap();
        std::fs::copy(wal_path(&path), wal_path(&copied)).unwrap();
        assert!(
            text_in(&copied).contains("again"),
            "a crash here keeps the words"
        );
        drop(new);
        assert!(text_in(&path).contains("again"));
    }

    /// Saving elsewhere closes the old connection after the new file exists at another path.
    #[test]
    fn saving_elsewhere_leaves_the_new_files_own_log_alone() {
        let dir = guard_dir();
        let path = dir.join("a.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("mine");
        another_copy(&dir.join("b.kame"), "theirs");
        std::fs::rename(dir.join("b.kame"), &path).unwrap();
        session.check_file();
        let mut reader = Session::open(&path).unwrap();
        reader.insert(" read");
        assert!(wal_path(&path).exists());
        session.save_as(&dir.join("mine.kame")).unwrap();
        assert!(wal_path(&path).exists());
        drop(reader);
        assert!(text_in(&path).contains("read"));
    }

    fn replaced_copy(session: &Session, text: &str) -> Document {
        let mut new = session.document().clone();
        new.blocks[0].text = text.to_string();
        new.blocks[0].spans.clear();
        new
    }

    #[test]
    fn replacing_the_document_is_one_undo_step() {
        let mut session = Session::anonymous().unwrap();
        session.insert("before");
        let new = replaced_copy(&session, "after");
        assert!(session.replace_document(new));
        assert_eq!(session.document().blocks[0].text, "after");
        session.undo();
        assert_eq!(session.document().blocks[0].text, "before");
        session.redo();
        assert_eq!(session.document().blocks[0].text, "after");
    }

    #[test]
    fn replacing_the_document_survives_a_reopen_and_can_still_be_undone() {
        let dir = guard_dir();
        let path = dir.join("a.kame");
        {
            let mut session = Session::open(&path).unwrap();
            session.insert("before");
            let new = replaced_copy(&session, "after");
            assert!(session.replace_document(new));
        }
        let mut session = Session::open(&path).unwrap();
        assert_eq!(session.document().blocks[0].text, "after");
        session.undo();
        assert_eq!(session.document().blocks[0].text, "before");
    }

    #[test]
    fn replacing_the_document_keeps_the_caret_on_something_real() {
        let mut session = Session::anonymous().unwrap();
        session.insert("a long first version");
        let new = replaced_copy(&session, "short");
        session.replace_document(new);
        assert_eq!(session.selection, None);
        assert_eq!(session.caret.offset, "short".len());
    }

    #[test]
    fn typing_after_replacing_starts_a_new_undo_step() {
        let mut session = Session::anonymous().unwrap();
        session.insert("before");
        let new = replaced_copy(&session, "after");
        session.replace_document(new);
        session.insert(" more");
        session.undo();
        assert_eq!(session.document().blocks[0].text, "after");
    }

    #[test]
    fn nothing_replaces_a_document_once_saving_has_failed() {
        let mut session = Session::anonymous().unwrap();
        session.insert("before");
        let new = replaced_copy(&session, "after");
        session.save_failed = true;
        assert!(!session.replace_document(new));
        assert_eq!(session.document().blocks[0].text, "before");
    }

    /// Four blocks citing `old`: a prose block with a multibyte letter, code, a link and plain
    /// words carrying a footnote, a marginal and a note that each cite it.
    fn citing_session() -> Session {
        let mut session = Session::anonymous().unwrap();
        session.insert("[old] [old2] é[old]");
        for text in ["[old]", "see [old] here", "plain words"] {
            session.split();
            session.insert(text);
        }
        let ids: Vec<BlockId> = session.document().blocks.iter().map(|b| b.id).collect();
        assert!(session.set_block_kind(ids[1], BlockKind::Code { language: None }));
        assert!(session.set_link(ids[2], 4, 9, "https://example.com".to_string()));
        session.set_caret(Cursor {
            block_id: ids[3],
            offset: 1,
        });
        for (kind, body) in [
            (AnnotationKind::Footnote, "a footnote [old] [old]"),
            (AnnotationKind::Marginal, "[old] in the margin"),
            (AnnotationKind::Note, "a private [old]"),
        ] {
            let id = session.add_annotation(kind).unwrap();
            assert!(session.set_annotation_body(id, body));
        }
        session
    }

    fn texts(session: &Session) -> Vec<String> {
        let document = session.document();
        let blocks = document.blocks.iter().map(|block| block.text.clone());
        let notes = document.annotations.iter().map(|note| note.body.clone());
        blocks.chain(notes).collect()
    }

    #[test]
    fn renaming_a_citation_leaves_everything_else_alone() {
        let mut session = citing_session();
        let before = texts(&session);
        let before_document = session.document().clone();
        let first = session.document().blocks[0].id;
        session.set_caret(Cursor {
            block_id: first,
            offset: before[0].len(),
        });
        assert_eq!(session.rename_citation("old", "fresh"), 5);
        let after = texts(&session);
        assert_eq!(after[0], "[fresh] [old2] é[fresh]");
        assert_eq!(after[1], "[old]", "code");
        assert_eq!(after[2], "see [old] here", "a link's words");
        assert_eq!(after[3], "plain words");
        assert_eq!(after[4], "a footnote [fresh] [fresh]");
        assert_eq!(after[5], "[fresh] in the margin");
        assert_eq!(after[6], "a private [old]", "a note");
        assert_eq!(
            session.caret.offset,
            after[0].len(),
            "the caret stays at the end"
        );
        // Whatever the rename didn't change is as it was.
        let document = session.document();
        assert_eq!(document.blocks[2].spans, before_document.blocks[2].spans);
        assert_eq!(document.blocks[1].kind, before_document.blocks[1].kind);
        // The whole rename is one step.
        assert!(session.undo());
        assert_eq!(texts(&session), before);
    }

    #[test]
    fn renaming_keeps_a_caret_in_the_middle_of_its_text() {
        let mut session = citing_session();
        let first = session.document().blocks[0].id;
        // In the last citation's key, and then after the first citation.
        let in_key = "[old] [old2] é[".len() + 1;
        session.set_caret(Cursor {
            block_id: first,
            offset: in_key,
        });
        session.rename_citation("old", "fresh");
        assert_eq!(session.caret.offset, "[fresh] [old2] é[fresh".len());
        session.set_caret(Cursor {
            block_id: first,
            offset: "[fresh] ".len(),
        });
        session.rename_citation("fresh", "x");
        assert_eq!(session.caret.offset, "[x] ".len());
    }

    #[test]
    fn renaming_to_the_same_or_an_invalid_key_changes_nothing() {
        let mut session = citing_session();
        let before = texts(&session);
        assert_eq!(session.rename_citation("old", "old"), 0);
        assert_eq!(session.rename_citation("old", "1bad"), 0);
        assert_eq!(session.rename_citation("absent", "fresh"), 0);
        assert_eq!(texts(&session), before);
    }

    fn anchors(session: &Session) -> Vec<(usize, usize)> {
        let ranges = session.document().annotations.iter().map(|note| {
            let (start, end) = note.anchor.byte_range().unwrap();
            (start, end)
        });
        ranges.collect()
    }

    /// One paragraph, `text`, with a note and a footnote added while the caret was at `at`.
    fn noted(text: &str, at: usize) -> Session {
        let mut session = Session::anonymous().unwrap();
        session.insert(text);
        let block_id = session.document().blocks[0].id;
        session.set_caret(Cursor {
            block_id,
            offset: at,
        });
        for (kind, body) in [
            (AnnotationKind::Note, "my note"),
            (AnnotationKind::Footnote, "my footnote"),
        ] {
            let id = session.add_annotation(kind).unwrap();
            assert!(session.set_annotation_body(id, body));
        }
        session
    }

    #[test]
    fn a_note_and_a_footnote_on_exactly_the_key_survive_and_cover_the_new_key() {
        let mut session = noted("see [smith2020] now", 8);
        assert_eq!(anchors(&session), [(5, 14), (5, 14)]);
        assert_eq!(session.rename_citation("smith2020", "li19"), 1);
        assert_eq!(session.document().blocks[0].text, "see [li19] now");
        assert_eq!(anchors(&session), [(5, 9), (5, 9)]);
        let bodies: Vec<&str> = session
            .document()
            .annotations
            .iter()
            .map(|note| note.body.as_str())
            .collect();
        assert_eq!(bodies, ["my note", "my footnote"]);
        assert_eq!(session.rename_citation("li19", "jones2001abc"), 1);
    }

    #[test]
    fn a_longer_and_a_shorter_key_move_everything_after_them() {
        let text = "éé [a] and [a] then [a] end";
        let key = |n: usize| text.match_indices("[a]").nth(n).unwrap().0 + 1;
        let mut session = noted(text, key(1));
        let bold_whole = Span {
            start: key(1) - 1,
            end: key(1) + 2,
            style: SpanStyle::Bold,
        };
        let bold_later = Span {
            start: key(2) - 1,
            end: key(2) + 2,
            style: SpanStyle::Bold,
        };
        let mut document = session.document().clone();
        document.blocks[0].spans = vec![bold_whole, bold_later];
        // A note on later text, and one around a whole citation.
        let id = document.blocks[0].id;
        document.annotations[0].anchor = Anchor::range(id, key(2) + 4, text.len());
        document.annotations[1].anchor = Anchor::range(id, key(0) - 1, key(0) + 2);
        assert!(session.replace_document(document));
        assert_eq!(session.rename_citation("a", "longer"), 3);
        let new = "éé [longer] and [longer] then [longer] end";
        assert_eq!(session.document().blocks[0].text, new);
        let whole = |n: usize| new.match_indices("[longer]").nth(n).unwrap().0;
        let spans = &session.document().blocks[0].spans;
        assert_eq!((spans[0].start, spans[0].end), (whole(1), whole(1) + 8));
        assert_eq!((spans[1].start, spans[1].end), (whole(2), whole(2) + 8));
        assert_eq!(
            anchors(&session),
            [(whole(2) + 8 + 2, new.len()), (whole(0), whole(0) + 8)]
        );
        assert_eq!(
            session.caret.offset,
            whole(1) + 1,
            "a caret at a key's start stays"
        );
        assert_eq!(session.rename_citation("longer", "z"), 3);
        let back = "éé [z] and [z] then [z] end";
        assert_eq!(session.document().blocks[0].text, back);
        assert_eq!(
            anchors(&session)[1],
            (back.find("[z]").unwrap(), back.find("[z]").unwrap() + 3)
        );
        // One undo returns the document whole, anchors and all.
        let before = session.document().clone();
        assert!(session.undo());
        assert_eq!(session.document().blocks[0].text, new);
        assert!(session.undo());
        assert_eq!(session.document().blocks[0].text, text);
        assert_ne!(session.document(), &before);
        assert_eq!(anchors(&session)[1], (key(0) - 1, key(0) + 2));
    }

    #[test]
    fn a_rename_that_changes_nothing_keeps_the_selection() {
        let mut session = noted("see [smith2020] now", 8);
        let id = session.document().blocks[0].id;
        session.set_caret(Cursor {
            block_id: id,
            offset: 1,
        });
        session.select_to(Cursor {
            block_id: id,
            offset: 4,
        });
        let (caret, selection) = (session.caret, session.selection);
        assert!(selection.is_some());
        assert_eq!(session.rename_citation("other", "fresh"), 0);
        assert_eq!((session.caret, session.selection), (caret, selection));
    }

    /// `text` in one paragraph, with a selection from `start` to `end`.
    fn selecting(text: &str, start: usize, end: usize) -> Session {
        let mut session = Session::anonymous().unwrap();
        session.insert(text);
        let block_id = session.document().blocks[0].id;
        session.set_caret(Cursor {
            block_id,
            offset: start,
        });
        session.select_to(Cursor {
            block_id,
            offset: end,
        });
        session
    }

    #[test]
    fn a_selection_wholly_inside_a_key_covers_the_renamed_key() {
        let text = "é [smith2020] now";
        let key = text.find("smith2020").unwrap();
        let (inside, later, end) = (key + 1, key + 4, key + "smith2020".len());
        let new_end = key + "li19".len();
        // Dragged forward and back, with one end inside or both, and with the moving end
        // where the caret is.
        for (from, to, caret) in [
            (inside, later, new_end),
            (later, inside, key),
            (inside, end, new_end),
            (end, inside, key),
        ] {
            let mut session = selecting(text, from, to);
            let before = session.document().clone();
            assert_eq!(session.rename_citation("smith2020", "li19"), 1);
            let (start_of, end_of) = session.ordered_selection().unwrap();
            assert_eq!((start_of.offset, end_of.offset), (key, new_end), "{from}");
            assert_eq!(session.caret.offset, caret, "{from}");
            assert_eq!(session.selection.unwrap().end, session.caret, "{from}");
            assert!(session.undo());
            assert_eq!(session.document(), &before);
        }
    }

    #[test]
    fn a_selection_outside_a_key_or_around_one_moves_with_the_text() {
        let text = "é [a] and [a] end";
        let key = |n: usize| text.match_indices("[a]").nth(n).unwrap().0 + 1;
        let new = "é [longer] and [longer] end";
        let new_key = |n: usize| new.match_indices("[longer]").nth(n).unwrap().0 + 1;
        let tail = text.len() - 1;
        for (from, to, want_from, want_to) in [
            // Before the keys, and after them.
            (0, key(0) - 1, 0, new_key(0) - 1),
            (tail - 2, tail, new.len() - 3, new.len() - 1),
            // Around one, exactly on it, and between two.
            (0, key(0) + 2, 0, new_key(0) + "longer]".len()),
            (key(0), key(0) + 1, new_key(0), new_key(0) + 6),
            (key(0) + 2, key(1) - 1, new_key(0) + 7, new_key(1) - 1),
            // From the middle of the document to the far end, either way round.
            (key(1), tail, new_key(1), new.len() - 1),
            (tail, key(1), new.len() - 1, new_key(1)),
        ] {
            let mut session = selecting(text, from, to);
            assert_eq!(session.rename_citation("a", "longer"), 2);
            assert_eq!(session.document().blocks[0].text, new);
            let selection = session.selection.unwrap();
            assert_eq!(
                (selection.start.offset, selection.end.offset),
                (want_from, want_to),
                "{from}..{to}"
            );
        }
    }

    #[test]
    fn a_selection_with_its_ends_in_two_keys_keeps_each_at_its_key_s_end() {
        // One letter into each key, which only a key of more than one letter has.
        let text = "[ab] and [ab] end";
        let key = |n: usize| text.match_indices("[ab]").nth(n).unwrap().0 + 1;
        let new = "[xyz] and [xyz] end";
        let new_key = |n: usize| new.match_indices("[xyz]").nth(n).unwrap().0 + 1;
        let mut session = selecting(text, key(0) + 1, key(1) + 1);
        assert_eq!(session.rename_citation("ab", "xyz"), 2);
        let selection = session.selection.unwrap();
        assert_eq!(
            (selection.start.offset, selection.end.offset),
            (new_key(0) + 3, new_key(1) + 3)
        );
    }

    #[test]
    fn a_selection_inside_keys_in_two_blocks_keeps_each_at_its_key_s_end() {
        let mut session = Session::anonymous().unwrap();
        session.insert("[ab]");
        assert!(session.split());
        session.insert("[ab]");
        let first = session.document().blocks[0].id;
        let second = session.document().blocks[1].id;
        session.set_caret(Cursor {
            block_id: first,
            offset: 2,
        });
        session.select_to(Cursor {
            block_id: second,
            offset: 2,
        });
        assert_eq!(session.rename_citation("ab", "xyz"), 2);
        let selection = session.selection.unwrap();
        assert_eq!(
            (selection.start.block_id, selection.start.offset),
            (first, 4)
        );
        assert_eq!((selection.end.block_id, selection.end.offset), (second, 4));
    }

    #[test]
    fn a_zero_width_anchor_stays_zero_width_through_a_rename() {
        let text = "see [smith2020] now";
        let key = text.find("smith2020").unwrap();
        let (end, new_end) = (key + "smith2020".len(), key + "li19".len());
        for (start_of, end_of, want) in [
            // Inside the key it goes where a caret there goes.
            (key + 3, key + 3, (new_end, new_end)),
            // At the key's edges, and either side of it, it moves with the text.
            (key, key, (key, key)),
            (end, end, (new_end, new_end)),
            (1, 1, (1, 1)),
            (end + 2, end + 2, (new_end + 2, new_end + 2)),
            // A range inside the key still comes to cover it.
            (key + 3, key + 5, (key, new_end)),
        ] {
            let mut session = noted(text, 0);
            let mut document = session.document().clone();
            let id = document.blocks[0].id;
            document.annotations[0].anchor = Anchor::range(id, start_of, end_of);
            assert!(session.replace_document(document));
            let before = session.document().clone();
            assert_eq!(session.rename_citation("smith2020", "li19"), 1);
            assert_eq!(anchors(&session)[0], want, "{start_of}..{end_of}");
            let after = session.document().clone();
            assert!(session.undo());
            assert_eq!(session.document(), &before, "{start_of}..{end_of}");
            assert!(session.redo());
            assert_eq!(session.document(), &after, "{start_of}..{end_of}");
        }
    }

    #[test]
    fn a_read_only_or_unsaveable_session_refuses_a_rename() {
        let mut session = citing_session();
        let before = texts(&session);
        session.newer = true;
        assert_eq!(session.rename_citation("old", "fresh"), 0);
        assert_eq!(texts(&session), before);
        session.newer = false;
        session.save_failed = true;
        assert_eq!(session.rename_citation("old", "fresh"), 0);
        assert_eq!(texts(&session), before);
    }
}
