//! Databases left behind on this machine after their document was moved or deleted on another. In
//! a folder with revision logs, Syncthing carries the logs and not the databases, so a database
//! finds out only by its logs having gone. See "The other machines" in
//! `docs/superpowers/specs/2026-11-04-notebook-file-operations-design.md`.
//!
//! This runs with nobody looking, so it only ever renames, never over anything, and does nothing
//! at all if some logs that might change the answer can't be read. What it doesn't settle on one
//! pass, it sees again on the next.
//!
//! It comes in two parts, since reading every log is slow, and a rename mustn't be on another thread
//! from whatever opens documents. [`decide`] only reads, and can be on any thread. [`apply`] does
//! the renames, on the thread that opens documents: `build_from_logs` puts a database in place over
//! whatever has got there since it looked, so a rename landing there would be lost, and a document
//! opened during one would be moved out from under its session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kame_core::model::Metadata;
use kame_core::storage::Db;
use kame_sync::logs::{log_folder, logging_root, stranded_verdict};
use kame_sync::merge::document_digest;
use kame_sync::revlog::Store;

use crate::list::{NOT_DOCUMENTS, OUTPUT};
use crate::manage::{is_database, is_stranded_database, sqlite_files};
use crate::search::same_file;
use crate::Notebook;

/// What a pass did to one database.
#[derive(Clone, Debug, PartialEq)]
pub enum Done {
    Moved {
        from: PathBuf,
        to: PathBuf,
    },
    Retired {
        from: PathBuf,
        to: PathBuf,
        /// The moved document already has a database at its new home.
        opened_there: bool,
        /// The retired file may hold what the logs don't, or there's no way to tell, as there
        /// isn't for a deleted document. False only if it holds nothing beyond them.
        unsynced: bool,
    },
    Returned {
        from: PathBuf,
        to: PathBuf,
    },
}

/// What a pass found to do. It's worked out from reading only, so this can come from any thread.
#[derive(Debug)]
pub struct Steps {
    root: PathBuf,
    retiring: Vec<Retiring>,
    moving: Vec<Going>,
    returning: Vec<Going>,
}

#[derive(Debug)]
struct Retiring {
    path: PathBuf,
    revision: String,
    /// What it held when `unsynced` was worked out. A stranded document stays editable and saves
    /// no new revision, so its revision can't say whether this has changed.
    contents: Option<Contents>,
    opened_there: bool,
    unsynced: bool,
}

/// A database's document, by its digest, and properties.
#[derive(Debug, PartialEq)]
struct Contents {
    document: String,
    metadata: Metadata,
}

/// A database with a home to go to, and the revision it had when that was worked out.
#[derive(Debug)]
struct Going {
    path: PathBuf,
    revision: String,
    home: PathBuf,
}

/// How many numbered names to try for a retired or moving database before leaving it for now.
const NAMES_TO_TRY: u32 = 1000;

/// Which databases were found not to be stranded, and what was true when they were, so that a
/// later pass reads their logs only if that has changed. Being stranded is never remembered, nor is
/// a verdict that couldn't be reached, so a change can only cause another read. It belongs to one
/// pass at a time, going with it to its thread and back. Starting again with an empty one is only
/// slower.
#[derive(Debug, Default)]
pub struct Healthy {
    databases: HashMap<PathBuf, LooksHealthy>,
}

/// What a database's healthy verdict rests on: the revision it remembers, and each log's name and
/// size. Logs are only ever added to, so this comes from looking at them, with no reading.
#[derive(Debug, PartialEq)]
struct LooksHealthy {
    revision: String,
    logs: Vec<(PathBuf, u64)>,
}

/// The databases in the notebook that have lost their logs, for now at least. A database's logs are
/// read only if it has no verdict in `healthy` that still holds. So that's only one that's stranded,
/// or has logs or a revision new since the last look.
pub fn find_stranded(notebook: &Notebook, healthy: &mut Healthy) -> Vec<PathBuf> {
    let known = std::mem::take(&mut healthy.databases);
    let mut stranded = Vec::new();
    for path in notebook.documents().into_iter().map(|entry| entry.path) {
        // Nothing above it turning logs on means its state says nothing, and nothing would ever
        // settle it.
        if !is_database(&path) || !path.is_file() || logging_root(&path).is_none() {
            continue;
        }
        // With no revision it has no state to lose.
        let Some(revision) = recorded_revision(&path) else {
            continue;
        };
        let folder = log_folder(&path);
        // Before any reading, so that logs added to meanwhile are read again next time.
        let looks = Store::log_sizes(&folder).map(|logs| LooksHealthy {
            revision: revision.clone(),
            logs,
        });
        if looks.is_some() && looks.as_ref() == known.get(&path) {
            healthy.databases.extend(looks.map(|looks| (path, looks)));
            continue;
        }
        match stranded_verdict(Some(&revision), &folder) {
            Some(true) => stranded.push(path),
            Some(false) => healthy.databases.extend(looks.map(|looks| (path, looks))),
            None => {}
        }
    }
    stranded
}

/// What to do about the databases in `may_act_on`, each of which is looked at again first, and any
/// retired database whose logs have appeared. There's no clock here: the caller says what has
/// waited long enough. This changes nothing.
pub fn decide(notebook: &Notebook, may_act_on: &[PathBuf]) -> Steps {
    let mut steps = Steps {
        root: notebook.root.clone(),
        retiring: Vec::new(),
        moving: Vec::new(),
        returning: Vec::new(),
    };
    let mut to_settle = may_act_on.to_vec();
    to_settle.sort();
    to_settle.dedup();
    to_settle.retain(|path| is_still_stranded(notebook, path));

    let mut walked = Walked::default();
    walk(notebook, &notebook.root, &mut walked);
    // A retired database stays for good, so having one can't be a reason to read every log on every
    // pass. It can only come back to logs with no database at their path.
    let may_come_back =
        !walked.set_aside.is_empty() && walked.documents.iter().any(|document| is_free(document));
    if to_settle.is_empty() && !may_come_back {
        return steps;
    }
    // A home in what can't be read can't be ruled out, and that would make a retirement a guess.
    if !walked.whole {
        return steps;
    }
    let Some(logs) = log_folders(&walked.documents) else {
        return steps;
    };

    for path in to_settle {
        let Some(revision) = recorded_revision(&path) else {
            continue;
        };
        let contents = contents_of(&path);
        match what_to_do(&path, &revision, contents.as_ref(), &logs) {
            Some(Verdict::Retire {
                opened_there,
                unsynced,
            }) => steps.retiring.push(Retiring {
                path,
                revision,
                contents,
                opened_there,
                unsynced,
            }),
            Some(Verdict::Move { home }) => steps.moving.push(Going {
                path,
                revision,
                home,
            }),
            None => {}
        }
    }
    steps.returning = those_coming_back(notebook, &walked.set_aside, &logs);
    steps
}

/// Does what was found, on the thread that opens documents, so that nothing can be built or opened
/// in the middle of a rename. `open` is what's open now, not when `decide` began. Time has passed
/// since then, so each step is looked at again just before it's taken, with no more reading than
/// that takes: it's skipped if the database has gone, is open, or remembers something else now, or
/// if something has arrived where it's going. A retirement is also skipped if what the database
/// holds has changed, since what it reports about that would be out of date. This returns what was
/// done, and what's skipped or fails is left to be seen again by the next pass.
pub fn apply(notebook: &Notebook, steps: Steps, open: Option<&Path>) -> Vec<Done> {
    if steps.root != notebook.root {
        return Vec::new();
    }
    let is_open = |path: &Path| open.is_some_and(|open| same_file(open, path));
    let as_it_was = |path: &Path, revision: &str| {
        !is_open(path) && recorded_revision(path).as_deref() == Some(revision)
    };

    let mut done = Vec::new();
    for step in steps.retiring {
        if !as_it_was(&step.path, &step.revision) || contents_of(&step.path) != step.contents {
            continue;
        }
        if let Some(to) = retired_name(&step.path) {
            if leave(&step.path, &to) {
                done.push(Done::Retired {
                    from: step.path,
                    to,
                    opened_there: step.opened_there,
                    unsynced: step.unsynced,
                });
            }
        }
    }
    // After the retirements, which may have freed a name that a mover is going to.
    let movers = steps
        .moving
        .into_iter()
        .filter(|mover| as_it_was(&mover.path, &mover.revision))
        .collect();
    done.extend(carry_out_moves(movers, &is_open));
    // After the moves, since a database moving is the document as it is now, and a retired one
    // isn't.
    for step in steps.returning {
        if as_it_was(&step.path, &step.revision)
            && !is_open(&step.home)
            && arrive(&step.path, &step.home)
        {
            done.push(Done::Returned {
                from: step.path,
                to: step.home,
            });
        }
    }
    done
}

/// Both, for callers with nothing else going on.
pub fn reconcile(notebook: &Notebook, may_act_on: &[PathBuf], open: Option<&Path>) -> Vec<Done> {
    apply(notebook, decide(notebook, may_act_on), open)
}

/// Whether a path is a database of this notebook that's stranded right now. `is_stranded` takes a
/// folder it can't list, or a file where the folder would be, for logs without the revision. Here
/// that's something that can't be told, so it isn't stranded.
fn is_still_stranded(notebook: &Notebook, path: &Path) -> bool {
    let plain_file = path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_file());
    let folder = log_folder(path);
    let logs_can_be_told = match folder.symlink_metadata() {
        Ok(metadata) => metadata.is_dir() && std::fs::read_dir(&folder).is_ok(),
        Err(err) => err.kind() == std::io::ErrorKind::NotFound,
    };
    plain_file
        && is_database(path)
        && notebook.holds(path)
        && notebook.in_documents_area(path)
        && logs_can_be_told
        && is_stranded_database(path)
}

/// The revision a database remembers syncing with last.
fn recorded_revision(path: &Path) -> Option<String> {
    Db::open_read_only(path).ok()?.load_sync_state()
}

/// What a walk of the notebook found.
struct Walked {
    /// The document each log folder stands for.
    documents: Vec<PathBuf>,
    /// Retired databases, and any left in a temporary name by a crash.
    set_aside: Vec<PathBuf>,
    /// Whether every folder it should have seen could be read.
    whole: bool,
}

impl Default for Walked {
    fn default() -> Walked {
        Walked {
            documents: Vec::new(),
            set_aside: Vec::new(),
            whole: true,
        }
    }
}

/// Goes through the folders `Notebook::entries` finds documents in, the same way: symbolic links
/// aren't followed, and hidden folders are passed over, which is also what keeps the old logs in a
/// sync tool's `.stversions` from being taken for a home.
fn walk(notebook: &Notebook, folder: &Path, walked: &mut Walked) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        walked.whole = false;
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            walked.whole = false;
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            walked.whole = false;
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if kind.is_dir() {
            let logs_of = name
                .strip_prefix('.')
                .and_then(|rest| rest.strip_suffix(".sync"));
            if let Some(document) = logs_of {
                if document.to_lowercase().ends_with(".kame") {
                    walked.documents.push(folder.join(document));
                }
                continue;
            }
            let no_documents = folder == notebook.root
                && (name == OUTPUT || NOT_DOCUMENTS.contains(&name.as_str()));
            if !name.starts_with('.') && !no_documents {
                walk(notebook, &entry.path(), walked);
            }
        } else if kind.is_file() && is_set_aside(&name) {
            walked.set_aside.push(entry.path());
        }
    }
}

/// Whether a file's name is `.X.retired.kame`, `.X.retired-2.kame` or `.X.moving-1.kame`.
fn is_set_aside(name: &str) -> bool {
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

/// One log folder, read.
struct Logs {
    document: PathBuf,
    store: Store,
}

/// The logs of each of these documents. `None` if any can't be read, or hold lines from a newer
/// kame, which are skipped when read, since either could be holding the revision being looked for.
fn log_folders(documents: &[PathBuf]) -> Option<Vec<Logs>> {
    documents
        .iter()
        .map(|document| {
            let folder = log_folder(document);
            std::fs::read_dir(&folder).ok()?;
            let store = Store::load(&folder).ok()?;
            (!store.holds_newer()).then(|| Logs {
                document: document.clone(),
                store,
            })
        })
        .collect()
}

/// The documents whose logs hold a revision.
fn homes_of<'a>(revision: &str, logs: &'a [Logs]) -> Vec<&'a Path> {
    logs.iter()
        .filter(|logs| logs.store.contains(revision))
        .map(|logs| logs.document.as_path())
        .collect()
}

enum Verdict {
    Retire { opened_there: bool, unsynced: bool },
    Move { home: PathBuf },
}

/// What's to be done with a stranded database, or `None` to leave it alone: it has more than one
/// home, its own logs have turned up since it was looked at, or what's at its home can't be read.
fn what_to_do(
    path: &Path,
    revision: &str,
    contents: Option<&Contents>,
    logs: &[Logs],
) -> Option<Verdict> {
    let homes = homes_of(revision, logs);
    let home = match homes[..] {
        [] => {
            return Some(Verdict::Retire {
                opened_there: false,
                unsynced: true,
            })
        }
        [home] if home != path => home,
        _ => return None,
    };
    let moves_in = Verdict::Move {
        home: home.to_path_buf(),
    };
    match home.symlink_metadata() {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(moves_in),
        Ok(metadata) if metadata.is_file() && Db::open_read_only(home).is_ok() => {
            if is_stranded_database(home) {
                // It may be leaving too, and if it isn't, this waits for it.
                Some(moves_in)
            } else {
                Some(Verdict::Retire {
                    opened_there: true,
                    unsynced: !is_only_what_logs_hold(contents, revision, logs),
                })
            }
        }
        _ => None,
    }
}

/// What a database holds, if that can be read.
fn contents_of(path: &Path) -> Option<Contents> {
    let db = Db::open_read_only(path).ok()?;
    Some(Contents {
        document: document_digest(&db.load_document().ok()?),
        metadata: db.load_metadata().ok()??,
    })
}

/// Whether a database's document and properties are just what the logs hold at its revision, the
/// same test a sync makes for whether there's anything to write. If that can't be read, it's no.
fn is_only_what_logs_hold(contents: Option<&Contents>, revision: &str, logs: &[Logs]) -> bool {
    let (Some(contents), Some(logs)) = (
        contents,
        logs.iter().find(|logs| logs.store.is_usable(revision)),
    ) else {
        return false;
    };
    contents.document == document_digest(&logs.store.document(revision))
        && contents.metadata == logs.store.metadata(revision)
}

/// Those that can go, which isn't all of them. A mover needs its home empty, or holding only
/// another mover, which then takes everything there along with it. Two wanting one home can't both
/// have it, and which it should be isn't known. Dropping a mover can leave another without a home
/// to go to, so this repeats until none drop. A swap stays in, since each makes room for the other.
fn those_that_can_go(mut movers: Vec<Going>, is_open: &impl Fn(&Path) -> bool) -> Vec<Going> {
    loop {
        let can_go: Vec<bool> = movers
            .iter()
            .map(|mover| {
                let wanting = movers.iter().filter(|other| other.home == mover.home);
                let vacated = movers.iter().any(|other| other.path == mover.home);
                wanting.count() == 1 && !is_open(&mover.home) && (vacated || is_free(&mover.home))
            })
            .collect();
        if can_go.iter().all(|can| *can) {
            return movers;
        }
        let mut can_go = can_go.into_iter();
        movers.retain(|_| can_go.next().unwrap_or(false));
    }
}

/// Moves in two rounds, each first to a temporary name and then to its home, so that a name freed
/// by one is free for another. One that can't reach its home goes back. If that's been taken by
/// now, it stays in the temporary name, and a later pass treats it as retired.
fn carry_out_moves(movers: Vec<Going>, is_open: &impl Fn(&Path) -> bool) -> Vec<Done> {
    let mut parked = Vec::new();
    for mover in those_that_can_go(movers, is_open) {
        if let Some(temporary) = temporary_name(&mover.path) {
            if leave(&mover.path, &temporary) {
                parked.push((mover, temporary));
            }
        }
    }
    let mut done = Vec::new();
    for (mover, temporary) in parked {
        if arrive(&temporary, &mover.home) {
            done.push(Done::Moved {
                from: mover.path,
                to: mover.home,
            });
        } else {
            arrive(&temporary, &mover.path);
        }
    }
    done
}

/// The retired databases to join logs that hold their revision, at a path with no database. A
/// database with the same home that's still stranded, waiting its turn or on the screen, is further
/// on than the retired one, so that home is its to take. Each of several retired ones wanting one
/// home stays as it is, as with movers.
fn those_coming_back(notebook: &Notebook, set_aside: &[PathBuf], logs: &[Logs]) -> Vec<Going> {
    let mut wanting: Vec<Going> = Vec::new();
    for path in set_aside {
        let Some(revision) = recorded_revision(path) else {
            continue;
        };
        if let [home] = homes_of(&revision, logs)[..] {
            wanting.push(Going {
                path: path.clone(),
                revision,
                home: home.to_path_buf(),
            });
        }
    }
    let alone = |home: &Path| wanting.iter().filter(|other| other.home == home).count() == 1;
    let is_coming_back: Vec<bool> = wanting
        .iter()
        .map(|going| alone(&going.home) && is_free(&going.home))
        .collect();
    let mut is_coming_back = is_coming_back.into_iter();
    wanting.retain(|_| is_coming_back.next().unwrap_or(false));
    if wanting.is_empty() {
        return wanting;
    }
    let spoken_for = revisions_of_stranded(notebook, logs);
    wanting.retain(|going| {
        !spoken_for
            .iter()
            .any(|revision| homes_of(revision, logs).contains(&going.home.as_path()))
    });
    wanting.sort_by(|one, other| one.path.cmp(&other.path));
    wanting
}

/// What each stranded database remembers, going by logs that have already been read. Logs that
/// aren't among them aren't a folder this could read, so they don't hold the revision.
fn revisions_of_stranded(notebook: &Notebook, logs: &[Logs]) -> Vec<String> {
    notebook
        .documents()
        .into_iter()
        .map(|entry| entry.path)
        .filter(|path| is_database(path) && path.is_file() && logging_root(path).is_some())
        .filter_map(|path| {
            let revision = recorded_revision(&path)?;
            let own = logs.iter().find(|logs| logs.document == path);
            (!own.is_some_and(|logs| logs.store.contains(&revision))).then_some(revision)
        })
        .collect()
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Where a database goes to be retired: the first free name, or `None` if none can be found.
fn retired_name(path: &Path) -> Option<PathBuf> {
    let stem = stem_of(path);
    std::iter::once(format!(".{stem}.retired.kame"))
        .chain((2..NAMES_TO_TRY).map(|number| format!(".{stem}.retired-{number}.kame")))
        .map(|name| path.with_file_name(name))
        .find(|candidate| is_free(candidate))
}

/// Where a database waits between the rounds of a move. The name ends in `.kame` and starts with a
/// `.`, so that the sync tool and the listing ignore it if a crash leaves it there.
fn temporary_name(path: &Path) -> Option<PathBuf> {
    let stem = stem_of(path);
    (1..NAMES_TO_TRY)
        .map(|number| path.with_file_name(format!(".{stem}.moving-{number}.kame")))
        .find(|candidate| is_free(candidate))
}

/// Whether there's nothing at a path that a database could arrive at. That includes what SQLite
/// keeps beside one, since a `-wal` there would be read as the log of the file arriving. A name
/// that can't be checked isn't free.
fn is_free(path: &Path) -> bool {
    std::iter::once(path.to_path_buf())
        .chain(sqlite_files(path))
        .all(|file| {
            matches!(file.symlink_metadata(),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound)
        })
}

/// A database going from a name a document can have to one it can't. The `-wal` goes first: if a
/// crash came between them, what's stranded is a `-wal` under a name no database is ever given,
/// rather than one where a new database could arrive and read it as its own.
fn leave(from: &Path, to: &Path) -> bool {
    let [wal, shm] = side_file_moves(from, to);
    can_rename(from, to) && rename_all(vec![wal, shm, (from.to_path_buf(), to.to_path_buf())])
}

/// A database arriving at a name a document can have. The file goes first, for the same reason:
/// no `-wal` is ever there ahead of its database.
fn arrive(from: &Path, to: &Path) -> bool {
    let [wal, shm] = side_file_moves(from, to);
    can_rename(from, to) && rename_all(vec![(from.to_path_buf(), to.to_path_buf()), wal, shm])
}

/// Whether the database is still there, and nothing is where it's going.
fn can_rename(from: &Path, to: &Path) -> bool {
    from.is_file() && is_free(to)
}

fn side_file_moves(from: &Path, to: &Path) -> [(PathBuf, PathBuf); 2] {
    let [from_wal, from_shm] = sqlite_files(from);
    let [to_wal, to_shm] = sqlite_files(to);
    [(from_wal, to_wal), (from_shm, to_shm)]
}

/// Renames each that's there, in order, or puts them all back. The caller has checked that the
/// database is there, and that nothing is at any of the new names.
fn rename_all(moves: Vec<(PathBuf, PathBuf)>) -> bool {
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (old, new) in moves {
        if old.symlink_metadata().is_err() {
            continue;
        }
        if std::fs::rename(&old, &new).is_err() {
            for (old, new) in moved.iter().rev() {
                let _ = std::fs::rename(new, old);
            }
            return false;
        }
        moved.push((old, new));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_database_that_has_gone_is_not_renamed_and_its_side_files_stay() {
        let root = kame_test_dir::folder("stranded-gone-first");
        let (gone, to) = (root.join("gone.kame"), root.join(".gone.retired.kame"));
        let [wal, _] = sqlite_files(&gone);
        std::fs::write(&wal, "left").unwrap();
        assert!(!leave(&gone, &to));
        assert!(!arrive(&gone, &to));
        assert!(wal.is_file());
        assert!(!sqlite_files(&to)[0].exists());
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
}
