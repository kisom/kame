//! Revision logs meeting an open document: which documents are logged, and bringing a session and
//! its logs together. See section 13 of `docs/superpowers/specs/2026-11-03-sync-design.md`.

use std::path::{Path, PathBuf};

use kame_core::edit::{apply_inverse, Cursor};
use kame_core::links::{find_document, resolve_open_path};
use kame_core::model::{Document, Metadata};
use kame_core::session::Session;
use kame_core::storage::Db;

use crate::files::version_of;
use crate::merge::document_digest;
use crate::revlog::{Replica, Store};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The file that turns on logs for every document in its folder and below.
pub const MARKER: &str = ".kame-logs";

/// How many ops apart the checkpoints through a document's past are. This is part of what the
/// machines agree on, so changing it costs them an ancestor at the first sync after.
const CHECKPOINT_EVERY: usize = 50;

/// The nearest folder above `path` with the marker in it, which is what makes `path` logged.
pub fn logging_root(path: &Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    absolute
        .ancestors()
        .skip(1)
        .find(|folder| folder.join(MARKER).is_file())
        .map(Path::to_path_buf)
}

/// Where a document's logs live, beside it.
pub fn log_folder(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".sync");
    PathBuf::from(name)
}

/// Whether a document has logs while nothing turns them on. Its folder's marker has gone astray,
/// so if its database is still ignored by the sync tool, the document has stopped moving between
/// machines with nothing else to show for it.
pub fn orphaned_logs(path: &Path) -> bool {
    log_folder(path).is_dir() && logging_root(path).is_none()
}

/// A name for this machine's logs: the host, and enough at random that two machines can't share
/// one, since a log with two writers would break the premise. It's chosen once and then kept.
pub fn device_name(host: &str) -> String {
    let host = host.split('.').next().unwrap_or_default().to_lowercase();
    let host: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let host = if host.is_empty() { "machine" } else { &host };
    let tail = uuid::Uuid::new_v4().simple().to_string();
    format!("{host}-{}", &tail[..4])
}

/// What to call a device in words, without the part that makes it unique.
pub fn display_device(device: &str) -> &str {
    match device.rsplit_once('-') {
        Some((host, tail))
            if tail.len() == 4 && tail.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            host
        }
        _ => device,
    }
}

/// What a sync did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Revisions written to this machine's log.
    pub written: usize,
    /// Whether the document changed because of what arrived.
    pub merged: bool,
    /// Notes that arrived in the document.
    pub notes: usize,
    /// The machine the newest words came from, if it can be told.
    pub from: Option<String>,
}

/// An open document's logs, kept in memory so that a sync reads only what's new.
pub struct Logged {
    replica: Replica,
    store: Store,
    last: Option<String>,
    /// What the document and its properties were at the last sync.
    synced: Option<(Document, Metadata)>,
}

impl Logged {
    /// `None` unless `session` is a document in a marked folder, and one whose logs can be read.
    pub fn open(session: &Session, device: &str) -> Option<Logged> {
        let path = session.path()?;
        logging_root(path)?;
        let folder = log_folder(path);
        let store = Store::load(&folder).ok()?;
        Some(Logged {
            replica: Replica {
                folder,
                device: device.to_string(),
            },
            store,
            last: session.sync_state(),
            synced: None,
        })
    }

    /// Whether another machine's words may have arrived.
    pub fn has_news(&self) -> bool {
        self.store.has_news(&self.replica.folder)
    }

    /// Whether a sync would have anything to do, for asking often and cheaply.
    pub fn needs_sync(&self, session: &Session) -> bool {
        self.has_news()
            || !self.synced.as_ref().is_some_and(|(doc, metadata)| {
                doc == session.document() && *metadata == session.metadata
            })
    }

    /// Writes down what's on the screen if it has changed, merges whatever has arrived, and puts
    /// the result on the screen as one undo step. A merge that brings notes takes a snapshot
    /// first. A document that can't save, or whose file has been replaced, is left alone, since
    /// another part of kame is asking the author about that.
    pub fn sync(&mut self, session: &mut Session, now: i64) -> Result<Outcome> {
        if session.save_failed || session.replaced {
            return Ok(Outcome::default());
        }
        self.store.refresh(&self.replica.folder)?;
        let local = session.document().clone();
        let metadata = session.metadata.clone();
        // A first sync of text that's already here builds on it. Otherwise it starts from this
        // document's history, which is shared with any machine sharing that past.
        if self.last.is_none() {
            self.last = self.store.heads().into_iter().find(|head| {
                document_digest(&self.store.document(head)) == document_digest(&local)
                    && self.store.metadata(head) == metadata
            });
        }
        if self.last.is_none() {
            self.last = Some(self.write_history(session)?);
        }
        let synced = self.replica.sync_in(
            &mut self.store,
            &local,
            &metadata,
            self.last.as_deref(),
            now,
        )?;

        let mut outcome = Outcome {
            written: synced.written,
            ..Outcome::default()
        };
        if synced.doc != local {
            let added = synced
                .doc
                .annotations
                .iter()
                .filter(|note| !local.annotations.iter().any(|had| had.id == note.id))
                .count();
            if added > 0
                && session
                    .snapshot(Some("before merging from another machine"))
                    .is_none()
            {
                return Err("could not take a snapshot first, so nothing was merged".into());
            }
            let caret = carried_caret(session, &local, &synced.doc);
            if !session.replace_document(synced.doc) {
                return Err("could not save the merge".into());
            }
            session.set_caret(caret);
            outcome.merged = true;
            outcome.notes = added;
            outcome.from = self.newest_other_device(&synced.head);
        }
        if synced.metadata != metadata {
            session.set_metadata(synced.metadata);
        }
        session.set_sync_state(&synced.head);
        self.last = Some(synced.head);
        self.synced = Some((session.document().clone(), session.metadata.clone()));
        Ok(outcome)
    }
}

impl Logged {
    /// Writes checkpoints through the document's history, returning the latest. A machine with
    /// the same history writes the same ones, which gives a merge an ancestor near where they
    /// really parted. They fall at fixed places, then at every op after the last of those, so two
    /// machines that were in step when they opted in share a revision right up to the point.
    fn write_history(&mut self, session: &Session) -> Result<String> {
        let version = version_of(session);
        let mut states = Vec::new();
        let mut doc = version.doc;
        let last_fixed = version.ops.len() - version.ops.len() % CHECKPOINT_EVERY;
        for at in (0..=version.ops.len()).rev() {
            if at % CHECKPOINT_EVERY == 0 || at > last_fixed {
                states.push((at, doc.clone()));
            }
            if at > 0 {
                apply_inverse(&mut doc, &version.ops[at - 1]);
            }
        }
        let mut last = None;
        for (at, state) in states.into_iter().rev() {
            last = Some(
                self.replica
                    .write_checkpoint(&mut self.store, &state, last, at as i64)?,
            );
        }
        last.ok_or_else(|| "a history always has a start".into())
    }

    /// Who wrote the newest revision that `head` has and this machine hadn't seen.
    fn newest_other_device(&self, head: &str) -> Option<String> {
        let known = self
            .last
            .as_ref()
            .map(|last| self.store.ancestors(last))
            .unwrap_or_default();
        self.store
            .ancestors(head)
            .into_iter()
            .filter(|id| !known.contains(id) && self.store.contains(id))
            .filter(|id| {
                let device = self.store.device(id);
                !device.is_empty() && device != self.replica.device
            })
            .max_by_key(|id| (self.store.when(id), id.clone()))
            .map(|id| display_device(self.store.device(&id)).to_string())
    }
}

/// Where the caret belongs after its block's text changed from `local` to `merged`.
fn carried_caret(session: &Session, local: &Document, merged: &Document) -> Cursor {
    let caret = session.caret;
    let text_of = |doc: &Document| {
        doc.blocks
            .iter()
            .find(|block| block.id == caret.block_id)
            .map(|block| block.text.clone())
    };
    let (Some(old), Some(new)) = (text_of(local), text_of(merged)) else {
        return caret;
    };
    Cursor {
        block_id: caret.block_id,
        offset: carried_offset(&old, &new, caret.offset),
    }
}

/// Carries a byte offset from `old` to `new`, by what they have in common at each end. Inside
/// what changed, it goes to the start of the change.
fn carried_offset(old: &str, new: &str, offset: usize) -> usize {
    let prefix: usize = old
        .chars()
        .zip(new.chars())
        .take_while(|(one, other)| one == other)
        .map(|(one, _)| one.len_utf8())
        .sum();
    if offset <= prefix {
        return offset;
    }
    let suffix: usize = old[prefix..]
        .chars()
        .rev()
        .zip(new[prefix..].chars().rev())
        .take_while(|(one, other)| one == other)
        .map(|(one, _)| one.len_utf8())
        .sum();
    if offset >= old.len() - suffix {
        new.len() - (old.len() - offset)
    } else {
        prefix
    }
}

/// This machine's host name, for naming its logs.
pub fn this_host() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().to_string())
        .unwrap_or_default()
}

/// Turns on logs for a folder, returning the line that the sync tool's ignore file needs, on every
/// machine, for the folder's databases to stop travelling. `None` if no ignore file was found.
pub fn enable(folder: &Path) -> Result<Option<String>> {
    let folder = folder.canonicalize()?;
    std::fs::write(folder.join(MARKER), "")?;
    Ok(folder
        .ancestors()
        .find(|above| above.join(".stignore").is_file())
        .and_then(|root| folder.strip_prefix(root).ok())
        .map(|inside| {
            let inside = inside.display().to_string();
            if inside.is_empty() {
                "**.kame".to_string()
            } else {
                format!("/{inside}/**.kame")
            }
        }))
}

/// The file a document link leads to, as [`find_document`], building it first if it has only logs.
pub fn find_or_build(path: &str, base: &Path) -> Result<Option<PathBuf>> {
    if let Some(found) = find_document(path, base) {
        return Ok(Some(found));
    }
    let mut named = resolve_open_path(path, base);
    if named
        .extension()
        .is_none_or(|extension| extension != "kame")
    {
        let mut with_extension = named.into_os_string();
        with_extension.push(".kame");
        named = PathBuf::from(with_extension);
    }
    Ok(build_from_logs(&named)?.then_some(named))
}

/// Builds the database of a document that has logs and no file yet, as on a fresh machine,
/// returning whether it did. Nothing is written unless the logs hold a whole document.
pub fn build_from_logs(path: &Path) -> Result<bool> {
    if path.exists() || logging_root(path).is_none() {
        return Ok(false);
    }
    let store = Store::load(&log_folder(path))?;
    let heads = store.heads();
    // Several heads merge once the document is open, so any of them is a fair place to start.
    let Some(head) = heads.first() else {
        return Ok(false);
    };
    // Written beside the path and moved into place, so that a failure can't leave half a document.
    let mut scratch = path.as_os_str().to_os_string();
    scratch.push(".building");
    let scratch = PathBuf::from(scratch);
    let _ = std::fs::remove_file(&scratch);
    {
        let mut db = Db::open(&scratch)?;
        db.save_document(&store.document(head))?;
        db.save_metadata(&store.metadata(head))?;
        db.save_sync_state(head)?;
    }
    // Closing has folded the log into the file, and the name stays free of these if it hasn't.
    for suffix in ["-wal", "-shm"] {
        let mut side = scratch.clone().into_os_string();
        side.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(side));
    }
    std::fs::rename(&scratch, path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offset_is_carried_past_a_change_before_it() {
        assert_eq!(carried_offset("abc def", "xx abc def", 4), 7);
        assert_eq!(carried_offset("abc def", "abc def yy", 4), 4);
        assert_eq!(carried_offset("abc def", "abc zzz def", 7), 11);
    }

    #[test]
    fn an_offset_inside_a_change_goes_to_its_start() {
        assert_eq!(carried_offset("abc def ghi", "abc XYZ ghi", 5), 4);
    }

    #[test]
    fn an_offset_stays_on_a_character_boundary() {
        let (old, new) = ("é — x", "éé — x");
        let at = old.find('x').unwrap();
        let carried = carried_offset(old, new, at);
        assert_eq!(&new[carried..], "x");
    }
}
