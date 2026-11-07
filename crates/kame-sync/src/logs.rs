//! Revision logs meeting an open document: which documents are logged, and bringing a session and
//! its logs together. See section 13 of `docs/superpowers/specs/2026-11-03-sync-design.md`.

use std::path::{Component, Path, PathBuf};

use kame_core::edit::{try_apply_inverse, Cursor};
use kame_core::links::{find_document, resolve_open_path, split_document_target};
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

/// Where a document is, as a sync records it: the name of its logging root's folder and its path
/// from there, with `/` between folders. `None` if nothing above it turns logs on. It's relative so
/// that a database carried to another machine, where the notebook may be somewhere else, is still in
/// the same place. The root's name is in it so that a copy into another notebook isn't. A root named
/// differently on another machine, or renamed, is somewhere else until the next sync, which only
/// makes a database not stranded.
pub fn place_of(path: &Path) -> Option<String> {
    let root = logging_root(path)?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let inside = absolute.strip_prefix(&root).ok()?;
    let mut parts: Vec<String> = root
        .canonicalize()
        .ok()?
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .into_iter()
        .collect();
    let at_root = parts.len();
    for part in inside.components() {
        match part {
            Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
            Component::ParentDir if parts.len() > at_root => {
                parts.pop();
            }
            _ => {}
        }
    }
    Some(parts.join("/"))
}

/// Where a document's logs live: in a hidden folder beside it, so that a folder of documents
/// still looks like one.
pub fn log_folder(path: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(".sync");
    path.with_file_name(name)
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

/// Whether a file's name is one the clean-up sets a database aside under: `.X.retired.kame`,
/// `.X.retired-2.kame` or `.X.moving-1.kame`.
pub fn is_set_aside(name: &str) -> bool {
    let Some(inner) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_suffix(".kame"))
    else {
        return false;
    };
    let is_number = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if let Some((stem, number)) = inner.rsplit_once(".moving-") {
        return !stem.is_empty() && is_number(number);
    }
    let Some((stem, rest)) = inner.rsplit_once(".retired") else {
        return false;
    };
    !stem.is_empty() && (rest.is_empty() || rest.strip_prefix('-').is_some_and(is_number))
}

/// Whether a database is where it says it last synced, `place`, in a folder with logs. This is what
/// tells logs having left a database from a database having been copied somewhere there were none.
/// One that's been set aside counts wherever it says: it isn't a document any more, and logs of its
/// own would go to every machine and be taken there for a home.
pub fn is_where_it_synced(path: &Path, place: Option<&str>) -> bool {
    let Some(here) = place_of(path) else {
        return false;
    };
    place == Some(here.as_str())
        || path
            .file_name()
            .is_some_and(|name| is_set_aside(&name.to_string_lossy()))
}

/// Whether a database has lost the logs it was syncing with: it remembers a revision, it's at the
/// place it last synced, and the logs beside it don't hold that revision, because they're gone or
/// belong to another document now. This is the case after a document is deleted or moved on another
/// machine. Being wrong here hides someone's document, so it's false anywhere else, as for a copy
/// made by hand, and when it can't be told, for logs that can't be read, that came from a newer kame,
/// or with a corrupt line that could be holding the revision ([`Store::may_hide_its_own`]). Nothing
/// is done on a guess. A line that only fails its check says what it held, so it counts as holding
/// that revision, which is what keeps a document in an older format from being taken for stranded.
///
/// Every part of kame asking this asks here, with what the database says about itself: `document`
/// gives its document's id, asked only when there's a corrupt line, or `None` if it can't be read,
/// and then any corrupt line could be holding the revision.
pub fn is_stranded(
    path: &Path,
    revision: Option<&str>,
    place: Option<&str>,
    document: impl FnOnce() -> Option<uuid::Uuid>,
) -> bool {
    stranded_verdict(path, revision, place, document).unwrap_or(false)
}

/// The same, with `None` for when it can't be told, for a caller that remembers the answer.
pub fn stranded_verdict(
    path: &Path,
    revision: Option<&str>,
    place: Option<&str>,
    document: impl FnOnce() -> Option<uuid::Uuid>,
) -> Option<bool> {
    let Some(revision) = revision else {
        return Some(false);
    };
    if !is_where_it_synced(path, place) {
        return Some(false);
    }
    let folder = log_folder(path);
    if !folder.exists() {
        return Some(true);
    }
    let store = Store::load(&folder).ok()?;
    if store.holds_newer() {
        return None;
    }
    if store.could_hold(revision) {
        return Some(false);
    }
    if !store.corrupt_lines() {
        return Some(true);
    }
    // Lines saying they're another document's, as at a path used again for a new one, can't be
    // hiding this one's revision.
    let may_be_here = match document() {
        Some(document) => store.may_hide_its_own(document),
        None => true,
    };
    (!may_be_here).then_some(true)
}

/// What the app says of a database that remembers a revision and no place, with nothing beside it,
/// when it doesn't sync.
pub const UNPLACED: &str = "this document last synced before kame recorded where it was kept, and \
     its logs are gone, so it may have been moved or deleted on another machine. It isn't syncing. \
     Save As gives it logs of its own";

/// Whether a database remembers a revision and no place, and no line at all is beside it, as read
/// from the disk. It last synced before kame recorded places, and its logs have gone. That can't be
/// told from a copy made by hand, so it isn't stranded, but it may have been moved or deleted on
/// another machine, and its history written here would bring it back there. So it doesn't sync.
/// Logs holding its revision coming back beside it let it, and Save As gives it logs of its own.
pub fn is_unplaced(path: &Path, revision: Option<&str>, place: Option<&str>) -> bool {
    revision.is_some()
        && place.is_none()
        && Store::load(&log_folder(path)).is_ok_and(|store| store.holds_nothing())
}

/// Whether what's beside a database can be taken for its logs: there's nothing there, they hold the
/// revision it remembers, or a line in them says it's of this document. Otherwise they're another
/// document's, or can't be shown not to be, which may be so even when it isn't stranded, as when it
/// isn't where it last synced, or that can't be told. Nothing retires it for that. But writing its
/// history there could bring one document's words into another's logs, on every machine, so it
/// doesn't sync, and they aren't its to delete or move. A folder or a log that can't be read shows
/// nothing either.
///
/// `revision` is `None` for a database that has never synced. Its first sync merges its words with
/// what's there, whoever's that is, as documents created through a link on two machines rely on, and
/// that's an accepted limit. But what's there still isn't its to take away.
pub fn logs_can_be_its_own(path: &Path, document: uuid::Uuid, revision: Option<&str>) -> bool {
    let folder = log_folder(path);
    if !folder.exists() {
        return true;
    }
    if std::fs::read_dir(&folder).is_err() {
        return false;
    }
    Store::load(&folder).is_ok_and(|store| {
        revision.is_some_and(|revision| store.could_hold(revision)) || store.can_be_for(document)
    })
}

/// What a sync did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Whether the logs no longer belong to this database, so that nothing was written.
    pub stranded: bool,
    /// Whether the logs at its path may be another document's, as [`logs_can_be_its_own`] says, so
    /// that nothing was written, and it isn't known to be stranded, so that the clean-up leaves it
    /// alone.
    pub others_logs: bool,
    /// Whether it last synced before kame recorded where it was kept, and nothing at all is beside it,
    /// so that it may have been moved or deleted on another machine, and nothing was written, as
    /// [`is_unplaced`] says. Its history written here would bring it back on every machine.
    pub unplaced: bool,
    /// Revisions written to this machine's log.
    pub written: usize,
    /// Whether the document changed because of what arrived.
    pub merged: bool,
    /// Notes that arrived in the document.
    pub notes: usize,
    /// The machine the newest words came from, if it can be told. For a merge with another document,
    /// the machine that document's newest words came from.
    pub from: Option<String>,
    /// Whether what arrived was another document made at this name, merged in, as when two machines
    /// make a document from one link, or one is deleted and another made at its name.
    pub another_document: bool,
}

/// An open document's logs, kept in memory so that a sync reads only what's new.
pub struct Logged {
    replica: Replica,
    store: Store,
    last: Option<String>,
    /// Where the database says it last synced.
    place: Option<String>,
    /// Where it is now, by name and as a place.
    path: PathBuf,
    here: String,
    /// What the document and its properties were at the last sync.
    synced: Option<(Document, Metadata)>,
    /// Why the last sync wrote nothing, if that was because of what's at the log folder, with what
    /// that rested on.
    held_off: Option<HeldOff>,
    /// Whether the logs have left the database, as read on another thread, for the next sync.
    known: Option<StrandedAnswer>,
}

/// What reading every log beside a database takes, to find out whether they've left it: a sync with
/// news has to know that before it writes anything. Reading them is slow for long logs, so this can
/// go to another thread, and [`StrandedCheck::run`] there gives the answer, for [`Logged::know`].
#[derive(Debug)]
pub struct StrandedCheck {
    path: PathBuf,
    revision: String,
    place: Option<String>,
    document: uuid::Uuid,
    /// Each log's name and size from before any reading, which the answer rests on.
    logs: Option<Vec<(PathBuf, u64)>>,
}

/// Whether the logs have left a database, as [`is_stranded`] says, and what that was asked about.
#[derive(Debug)]
pub struct StrandedAnswer {
    check: StrandedCheck,
    stranded: bool,
}

impl StrandedCheck {
    /// Reads every log, on whatever thread this is.
    pub fn run(self) -> StrandedAnswer {
        let document = self.document;
        let stranded = is_stranded(
            &self.path,
            Some(&self.revision),
            self.place.as_deref(),
            || Some(document),
        );
        StrandedAnswer {
            check: self,
            stranded,
        }
    }
}

impl StrandedAnswer {
    /// Whether this answers what a sync of `logged`, for `document`, would ask now, with the logs as
    /// `logs` says.
    fn holds_for(
        &self,
        logged: &Logged,
        document: uuid::Uuid,
        logs: Option<&Vec<(PathBuf, u64)>>,
    ) -> bool {
        let check = &self.check;
        check.path == logged.path
            && logged.last.as_ref() == Some(&check.revision)
            && check.place == logged.place
            && check.document == document
            && check.logs.as_ref() == logs
    }
}

/// A sync that wrote nothing because the logs have left the database, or may be another document's,
/// and each log's name and size when it found that out, from looking at them and reading none. Asked
/// again while they're unchanged, for the same document, it's the same answer, and nothing is read
/// for it. The answer also depends on the database's revision and place, which don't change while it
/// isn't syncing. Its document's id can, as when a merge with another document is undone, so that's
/// kept with it.
#[derive(Debug, PartialEq)]
struct HeldOff {
    stranded: bool,
    /// Whether it's for having no place and nothing beside it, rather than for another document's
    /// logs, when it isn't stranded.
    unplaced: bool,
    document: uuid::Uuid,
    logs: Option<Vec<(PathBuf, u64)>>,
}

impl Logged {
    /// `None` unless `session` is a document in a marked folder, and one whose logs can be read.
    pub fn open(session: &Session, device: &str) -> Option<Logged> {
        Logged::try_open(session, device).ok()
    }

    /// [`Logged::open`], saying why when it can't.
    pub fn try_open(session: &Session, device: &str) -> Result<Logged> {
        let path = session.path().ok_or("it has no file")?;
        let here = place_of(path).ok_or("its place in the folder can't be told")?;
        let folder = log_folder(path);
        let store = Store::load(&folder)?;
        Ok(Logged {
            replica: Replica {
                folder,
                device: device.to_string(),
            },
            store,
            last: session.sync_state(),
            place: session.sync_place(),
            path: path.to_path_buf(),
            here,
            synced: None,
            held_off: None,
            known: None,
        })
    }

    /// Whether another machine's words may have arrived.
    pub fn has_news(&self) -> bool {
        self.store.has_news(&self.replica.folder)
    }

    /// Whether a sync has to find out if the logs have left this database before it can write: when
    /// it has a revision to lose, and its logs are gone, or something may have arrived, or this is
    /// the first sync and the logs as read when this opened don't hold the revision, as when it's
    /// been opened on another document's logs, where there's no news to see. The store keeps
    /// revisions after their files go, so it's the disk that's asked, and only when something could
    /// have changed.
    fn must_ask(&self) -> bool {
        let not_held = self.synced.is_none()
            && self
                .last
                .as_ref()
                .is_some_and(|last| !self.store.could_hold(last));
        self.last.is_some() && (!self.replica.folder.exists() || self.has_news() || not_held)
    }

    /// What the next sync would have to read first, if anything: every log, when it has to find out
    /// whether they've left this database, and doesn't already know. Logs that are gone take no
    /// reading to tell, and nor does what a held-off sync found while they're unchanged.
    pub fn stranded_check(&self, session: &Session) -> Option<StrandedCheck> {
        if !self.must_ask() || !self.replica.folder.exists() {
            return None;
        }
        let document = session.document().id;
        let logs = Store::log_sizes(&self.replica.folder);
        let answered = self
            .held_off
            .as_ref()
            .is_some_and(|held_off| held_off.holds_for(document, &logs))
            || self
                .known
                .as_ref()
                .is_some_and(|known| known.holds_for(self, document, logs.as_ref()));
        if answered {
            return None;
        }
        Some(StrandedCheck {
            path: self.path.clone(),
            revision: self.last.clone()?,
            place: self.place.clone(),
            document,
            logs,
        })
    }

    /// Takes an answer to [`Logged::stranded_check`], for the next sync to go by instead of reading
    /// the logs itself, if they're still as they were when it was asked. One for something else, as
    /// after a sync since, is let go.
    pub fn know(&mut self, answer: StrandedAnswer) {
        let current = answer.check.path == self.path
            && self.last.as_ref() == Some(&answer.check.revision)
            && answer.check.place == self.place;
        if current {
            self.known = Some(answer);
        }
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
    ///
    /// If the logs hold something from a newer version of kame, this still writes down what's on
    /// the screen, and then the document goes read-only, since this build can't see what's arrived.
    pub fn sync(&mut self, session: &mut Session, now: i64) -> Result<Outcome> {
        let outcome = self.write_and_merge(session, now);
        if self.store.holds_newer() && !session.newer {
            session.mark_newer();
        }
        outcome
    }

    fn write_and_merge(&mut self, session: &mut Session, now: i64) -> Result<Outcome> {
        // A view of a newer version's document leaves out what this build doesn't know, so neither
        // it nor a merge with it may be written anywhere. It can't save either, which covers it,
        // but it's said here as well, since writing it would lose what the newer version wrote.
        if session.save_failed || session.replaced || session.lossy {
            return Ok(Outcome::default());
        }
        // Before any reading, so that logs added to meanwhile are read next time.
        let logs = Store::log_sizes(&self.replica.folder);
        let document = session.document().id;
        if let Some(held_off) = self.held_off.take() {
            if held_off.holds_for(document, &logs) {
                let outcome = held_off.outcome();
                self.held_off = Some(held_off);
                return Ok(outcome);
            }
        }
        // Before any refresh or write, which could recreate a folder. An answer read on another
        // thread is what's gone by, if the logs are as they were when it was read.
        let known = self.known.take();
        if self.must_ask()
            && known
                .filter(|known| known.holds_for(self, document, logs.as_ref()))
                .map_or_else(
                    || {
                        is_stranded(
                            &self.path,
                            self.last.as_deref(),
                            self.place.as_deref(),
                            || Some(document),
                        )
                    },
                    |known| known.stranded,
                )
        {
            let held_off = HeldOff {
                stranded: true,
                unplaced: false,
                document,
                logs,
            };
            let outcome = held_off.outcome();
            self.held_off = Some(held_off);
            return Ok(outcome);
        }
        self.store.refresh(&self.replica.folder)?;
        // Fitted as what the logs give back is, or an undo that brought back offsets as an older
        // history recorded them would look like a merge.
        let mut local = session.document().clone();
        local.fit();
        let metadata = session.metadata.clone();
        // With no place and nothing here, it may have been moved or deleted elsewhere, and can't be
        // told from a copy, so its history written here could bring it back on every machine.
        let unplaced = self.last.is_some() && self.place.is_none() && self.store.holds_nothing();
        if unplaced
            || self.last.as_ref().is_some_and(|last| {
                !self.store.could_hold(last) && !self.store.can_be_for(local.id)
            })
        {
            let held_off = HeldOff {
                stranded: false,
                unplaced,
                document,
                logs,
            };
            let outcome = held_off.outcome();
            self.held_off = Some(held_off);
            return Ok(outcome);
        }
        // A first sync of text that's already here builds on it. Otherwise it starts from this
        // document's history, which is shared with any machine sharing that past.
        // What's remembered may not be in these logs, for a database copied here or last synced by
        // a kame that recorded no place, or may be missing what it needs.
        self.last = self.last.take().filter(|last| self.store.is_usable(last));
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
        // Before the sync state is saved, which SQLite puts on the disk itself. Were the state to
        // get there ahead of the revision it names, a power cut would leave a database remembering
        // a revision its logs don't hold, which reads as the logs having left, and the clean-up
        // would retire a healthy document.
        self.replica.flush(&mut self.store)?;

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
            // A merge bringing notes stands alone, after its snapshot, to be undone as itself.
            let saved = if added > 0 {
                session.replace_document(synced.doc)
            } else {
                session.follow_document(synced.doc)
            };
            if !saved {
                return Err("could not save the merge".into());
            }
            session.set_caret(caret);
            outcome.merged = true;
            outcome.notes = added;
            outcome.from = self.newest_other_device(&synced.head);
            if let Some(from) = self.another_document(&synced.head, local.id) {
                outcome.another_document = true;
                outcome.from = from;
            }
        }
        if synced.metadata != metadata {
            session.set_metadata(synced.metadata);
        }
        // Together. Were the place to get there alone, a database would be at its place naming a
        // revision from somewhere else, which reads as the logs having left.
        session.set_sync(&synced.head, &self.here);
        self.place = Some(self.here.clone());
        self.last = Some(synced.head);
        self.synced = Some((session.document().clone(), session.metadata.clone()));
        Ok(outcome)
    }
}

impl HeldOff {
    /// Whether this answers a sync of `document` now, with the logs as `logs` says.
    fn holds_for(&self, document: uuid::Uuid, logs: &Option<Vec<(PathBuf, u64)>>) -> bool {
        self.document == document && self.logs == *logs
    }

    fn outcome(&self) -> Outcome {
        Outcome {
            stranded: self.stranded,
            others_logs: !self.stranded && !self.unplaced,
            unplaced: self.unplaced,
            ..Outcome::default()
        }
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
            // A history that can't be rewound any further just has no checkpoints before that.
            if at > 0 && !try_apply_inverse(&mut doc, &version.ops[at - 1]) {
                break;
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

    /// Whether `head` brings revisions of a document that neither `document` nor anything this
    /// machine had seen is of: another document made at this name, merged in. If so, the machine
    /// that wrote the newest of them, when that can be told. A document that took another's id in
    /// an earlier merge, or gave it back by undoing one, has both in what it had seen, so it isn't
    /// told again.
    fn another_document(&self, head: &str, document: uuid::Uuid) -> Option<Option<String>> {
        let known = self
            .last
            .as_ref()
            .map(|last| self.store.ancestors(last))
            .unwrap_or_default();
        let mut held: std::collections::HashSet<uuid::Uuid> = known
            .iter()
            .filter(|id| self.store.contains(id))
            .map(|id| self.store.document_of(id))
            .collect();
        held.insert(document);
        let theirs: Vec<String> = self
            .store
            .ancestors(head)
            .into_iter()
            .filter(|id| {
                !known.contains(id)
                    && self.store.contains(id)
                    && !held.contains(&self.store.document_of(id))
            })
            .collect();
        if theirs.is_empty() {
            return None;
        }
        Some(
            theirs
                .into_iter()
                .filter(|id| !self.store.device(id).is_empty())
                .max_by_key(|id| (self.store.when(id), id.clone()))
                .map(|id| display_device(self.store.device(&id)).to_string()),
        )
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

/// Turns on logs for a folder, returning the lines that the sync tool's ignore file needs, on every
/// machine, for the folder's databases to stop travelling. That has to include what SQLite keeps
/// beside them, since a `-wal` from another machine would override the file it lands beside. `None`
/// if no ignore file was found.
pub fn enable(folder: &Path) -> Result<Option<Vec<String>>> {
    let folder = folder.canonicalize()?;
    std::fs::write(folder.join(MARKER), "")?;
    Ok(folder
        .ancestors()
        .find(|above| above.join(".stignore").is_file())
        .and_then(|root| folder.strip_prefix(root).ok())
        .map(|inside| {
            let inside = inside.display().to_string();
            let databases = if inside.is_empty() {
                "**.kame".to_string()
            } else {
                format!("/{inside}/**.kame")
            };
            vec![
                databases,
                "*.kame-wal".to_string(),
                "*.kame-shm".to_string(),
            ]
        }))
}

/// The file a document link leads to, as [`find_document`], building it first if it has only logs.
pub fn find_or_build(path: &str, base: &Path) -> Result<Option<PathBuf>> {
    if let Some(found) = find_document(path, base) {
        return Ok(Some(found));
    }
    let named = database_named(path, base);
    Ok(build_from_logs(&named)?.then_some(named))
}

/// Whether a link, written in the document at `base`, names a document that has logs and no file,
/// which [`find_or_build`] would build.
pub fn names_logs_only(target: &str, base: &Path) -> bool {
    let (path, _) = split_document_target(target);
    !path.is_empty()
        && find_document(path, base).is_none()
        && log_folder(&database_named(path, base)).is_dir()
}

/// The database a link's path names, relative to the document at `base`, with `.kame` added if
/// it was left off.
fn database_named(path: &str, base: &Path) -> PathBuf {
    let named = resolve_open_path(path, base);
    if named
        .extension()
        .is_some_and(|extension| extension == "kame")
    {
        return named;
    }
    let mut with_extension = named.into_os_string();
    with_extension.push(".kame");
    PathBuf::from(with_extension)
}

/// Why a document with only logs, none of whose revisions this build can use since a newer version
/// of kame wrote them, can't be opened.
pub const NEWER_LOGS: &str =
    "its logs hold only changes from a newer version of kame, so it can't \
     be opened here until kame is updated";

/// Why a document with only logs, whose revisions all build on others that aren't there, can't be
/// opened. They may be on the way, or gone for good with a move or a delete on another machine. The
/// machine these changes came from, which the log holding them is named for, still has what they
/// build on, and opening it there writes its history again, so that comes before deleting it here,
/// which removes what's left. Delete is in a notebook's browser, so the folder by hand is said too,
/// for a logged folder that isn't a notebook.
pub const WAITING_LOGS: &str = "its logs hold only changes that build on others that aren't here, \
     which may still arrive. If it was moved or deleted on another machine they won't, and opening it \
     on the machine these changes came from, which its log is named for, brings the rest here. To \
     remove what's left, delete it in the notebook's browser, or delete its hidden .sync folder by \
     hand";

/// Why a document with only logs, none of whose lines this build can use, can't be opened.
pub const UNUSABLE_LOGS: &str = "its logs hold no change this version of kame can use, only lines \
     that are damaged or that it can't check. If they're damaged, delete it in the notebook's \
     browser, or delete its hidden .sync folder by hand";

/// Builds the database of a document that has logs and no file yet, as on a fresh machine,
/// returning whether it did. Nothing is written unless the logs hold a whole document. This doesn't
/// need the folder's marker: without it the document won't sync, and says so, but opening it would
/// otherwise make a blank one where the logs have the text.
///
/// Logs holding no revision this build can use, and something from a newer version, are refused
/// with [`NEWER_LOGS`]: the document is all in what this build can't read, and a blank one made at
/// its name would be taken for it. Logs whose revisions all build on others that aren't there, as
/// a log arriving ahead of the one it builds on, or one left at a document's old name after a move,
/// are refused with [`WAITING_LOGS`] for the same reason: a blank document made there would sync
/// to every machine. So are logs holding lines and no revision this build can use, every one
/// damaged, holding a value reading has to fit, or failing its check as an older format's does,
/// with [`UNUSABLE_LOGS`]. Only logs holding no line at all, or only a tear, build nothing and are
/// opened as a new document.
pub fn build_from_logs(path: &Path) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    let store = Store::load(&log_folder(path))?;
    let heads = store.heads();
    // Several heads merge once the document is open, so any of them is a fair place to start.
    let Some(head) = heads.first() else {
        if store.holds_newer() {
            return Err(NEWER_LOGS.into());
        }
        if store.revision_count() > 0 {
            return Err(WAITING_LOGS.into());
        }
        if !store.holds_nothing() {
            return Err(UNUSABLE_LOGS.into());
        }
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
        match place_of(path) {
            Some(place) => db.save_sync(head, &place)?,
            None => db.save_sync_state(head)?,
        }
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
    fn a_link_names_a_document_there_only_as_logs() {
        let root = kame_test_dir::folder("logs-only-link");
        std::fs::create_dir_all(root.join("one")).unwrap();
        std::fs::create_dir_all(root.join("two/.d.kame.sync")).unwrap();
        std::fs::write(root.join("two/e.kame"), "").unwrap();
        std::fs::create_dir_all(root.join("two/.e.kame.sync")).unwrap();
        let base = root.join("one/a.kame");
        for link in ["../two/d", "../two/d.kame", "../two/d.kame#Part"] {
            assert!(names_logs_only(link, &base), "{link}");
        }
        for link in ["../two/e", "../two/f", "#Part", ""] {
            assert!(!names_logs_only(link, &base), "{link}");
        }
    }

    #[test]
    fn retired_and_temporary_names_are_told_whole() {
        for name in [
            ".a.retired.kame",
            ".a.retired-2.kame",
            ".a.moving-1.kame",
            ".v1.2.retired.kame",
            ".a.retired.retired.kame",
        ] {
            assert!(is_set_aside(name), "{name}");
        }
        for name in [
            "a.retired.kame",
            ".a.kame",
            ".retired.kame",
            ".moving-1.kame",
            ".a.retired-.kame",
            ".a.retired-x.kame",
            ".a.retiredx.kame",
            ".a.moving-.kame",
            ".a.moving.kame",
            ".a.retired.kame-wal",
        ] {
            assert!(!is_set_aside(name), "{name}");
        }
    }

    #[test]
    fn enabling_logs_says_every_line_the_ignore_file_needs() {
        let root = kame_test_dir::folder("logs-enable");
        std::fs::write(root.join(".stignore"), "").unwrap();
        let inner = root.join("writing/garden");
        std::fs::create_dir_all(&inner).unwrap();
        let lines = enable(&inner).unwrap().unwrap();
        assert_eq!(
            lines,
            vec![
                "/writing/garden/**.kame".to_string(),
                "*.kame-wal".to_string(),
                "*.kame-shm".to_string(),
            ]
        );
        assert!(inner.join(MARKER).is_file());
        let at_root = enable(&root).unwrap().unwrap();
        assert_eq!(at_root[0], "**.kame");
        assert_eq!(at_root.len(), 3);
    }

    /// The marker going astray stops the syncing, and there's a warning for that. It shouldn't also
    /// leave a blank document where the logs have the text.
    #[test]
    fn a_document_with_logs_and_no_marker_is_built_from_them() {
        let root = kame_test_dir::folder("logs-no-marker");
        let path = root.join("essay.kame");
        let mut doc = Document::new("t");
        doc.blocks[0].text = "The words.".to_string();
        Replica {
            folder: log_folder(&path),
            device: "elsewhere-0000".to_string(),
        }
        .sync(&doc, None, 1)
        .unwrap();
        assert!(build_from_logs(&path).unwrap());
        let session = Session::open(&path).unwrap();
        assert_eq!(session.document().blocks[0].text, "The words.");
    }

    /// What was typed goes in the log first, so nothing is lost, and then the document stops.
    #[test]
    fn logs_from_a_newer_build_make_the_document_read_only() {
        let root = kame_test_dir::folder("logs-newer");
        std::fs::write(root.join(MARKER), "").unwrap();
        let path = root.join("essay.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("Typed here.");
        let folder = log_folder(&path);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("new-0000.log"),
            "{\"id\":\"x\",\"revision\":{\"parents\":[],\"device\":\"new-0000\",\"when\":1,\"document\":\"00000000-0000-0000-0000-000000000001\",\"title\":\"t\",\"order\":[],\"annotations\":[],\"metadata\":{},\"future\":1},\"blocks\":[]}\n",
        )
        .unwrap();

        let mut logged = Logged::open(&session, "here-0000").unwrap();
        logged.sync(&mut session, 100).unwrap();
        assert!(session.newer);
        assert!(!session.insert("Refused."));
        let store = Store::load(&folder).unwrap();
        let heads = store.heads();
        assert_eq!(heads.len(), 1);
        assert_eq!(store.document(&heads[0]).blocks[0].text, "Typed here.");
    }

    /// A sync state on the disk ahead of its revision would, after a power cut, read as logs that
    /// have left, and retire a healthy document. A power cut can't be had in a test, so this
    /// checks that nothing is waiting to be flushed by the time the state has been saved.
    #[test]
    fn a_sync_records_its_state_with_its_log_already_flushed() {
        let root = kame_test_dir::folder("logs-flushed");
        std::fs::write(root.join(MARKER), "").unwrap();
        let path = root.join("essay.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("Typed here.");
        let mut logged = Logged::open(&session, "here-0000").unwrap();
        logged.sync(&mut session, 100).unwrap();

        assert!(!logged.store.has_unflushed());
        let state = session.sync_state().expect("it records where it got to");
        assert!(Store::load(&log_folder(&path)).unwrap().contains(&state));

        session.insert(" More.");
        logged.sync(&mut session, 101).unwrap();
        assert!(!logged.store.has_unflushed());
        let later = session.sync_state().unwrap();
        assert_ne!(later, state);
        assert!(Store::load(&log_folder(&path)).unwrap().contains(&later));
    }

    /// A file whose history can't be rewound still syncs, starting from the text it has.
    #[test]
    fn a_history_that_does_not_fit_stops_the_checkpoints_and_not_the_sync() {
        use kame_core::edit::{Editor, Op};
        let root = kame_test_dir::folder("logs-unfit-history");
        std::fs::write(root.join(MARKER), "").unwrap();
        let path = root.join("essay.kame");
        let mut session = Session::open(&path).unwrap();
        session.insert("words");
        let doc = session.document().clone();
        let stale = Op::InsertText {
            block_id: doc.blocks[0].id,
            offset: 0,
            text: "elsewhere".to_string(),
        };
        session.editor = Editor::with_cursor(doc.clone(), vec![stale], 1);
        let mut logged = Logged::open(&session, "here-0000").unwrap();
        logged.sync(&mut session, 100).unwrap();
        assert_eq!(*session.document(), doc);
        let store = Store::load(&log_folder(&path)).unwrap();
        assert_eq!(store.heads().len(), 1);
        assert_eq!(store.document(&store.heads()[0]), doc);
    }

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
