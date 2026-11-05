//! Databases left behind on this machine after their document was moved or deleted on another. In
//! a folder with revision logs, Syncthing carries the logs and not the databases, so a database
//! finds out only by its logs having gone. See "The other machines" in
//! `docs/superpowers/specs/2026-11-04-notebook-file-operations-design.md`.
//!
//! This runs with nobody looking, so it only ever renames, never over anything, and does nothing
//! at all if some logs that might change the answer can't be read. What it doesn't settle on one
//! pass, it sees again on the next.

use std::path::{Path, PathBuf};

use kame_core::storage::Db;
use kame_sync::logs::log_folder;
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
    /// `opened_there` if the moved document already had a database, and it isn't lost words.
    Retired {
        from: PathBuf,
        to: PathBuf,
        opened_there: bool,
    },
    Returned {
        from: PathBuf,
        to: PathBuf,
    },
}

/// How many numbered names to try for a retired or moving database before leaving it for now.
const NAMES_TO_TRY: u32 = 1000;

/// The databases in the notebook that have lost their logs, for now at least.
pub fn find_stranded(notebook: &Notebook) -> Vec<PathBuf> {
    notebook
        .documents()
        .into_iter()
        .map(|entry| entry.path)
        .filter(|path| is_database(path) && path.is_file() && is_stranded_database(path))
        .collect()
}

/// Settles the databases in `may_act_on`, each of which is looked at again first, and brings back
/// any retired database whose logs have appeared. There's no clock here: the caller says what has
/// waited long enough. Nothing at `open` is touched or put there. This returns what was done, and
/// anything that failed is left to be seen again by the next pass.
pub fn reconcile(notebook: &Notebook, may_act_on: &[PathBuf], open: Option<&Path>) -> Vec<Done> {
    let is_open = |path: &Path| open.is_some_and(|open| same_file(open, path));
    let mut to_settle = may_act_on.to_vec();
    to_settle.sort();
    to_settle.dedup();
    to_settle.retain(|path| !is_open(path) && is_still_stranded(notebook, path));

    let mut walked = Walked::default();
    walk(notebook, &notebook.root, &mut walked);
    if to_settle.is_empty() && walked.set_aside.is_empty() {
        return Vec::new();
    }
    // A home in what can't be read can't be ruled out, and that would make a retirement a guess.
    if !walked.whole {
        return Vec::new();
    }
    let Some(logs) = log_folders(&walked.documents) else {
        return Vec::new();
    };

    let mut done = Vec::new();
    let mut movers = Vec::new();
    for path in to_settle {
        let Some(revision) = recorded_revision(&path) else {
            continue;
        };
        match what_to_do(&path, &revision, &logs) {
            Some(Verdict::Retire { opened_there }) => {
                if let Some(to) = retired_name(&path) {
                    if leave(&path, &to) {
                        done.push(Done::Retired {
                            from: path,
                            to,
                            opened_there,
                        });
                    }
                }
            }
            Some(Verdict::Move { home }) => movers.push(Mover { path, home }),
            None => {}
        }
    }
    // After the retirements, which may have freed a name that a mover is going to.
    done.extend(carry_out_moves(movers, &is_open));
    // After the moves, since a database moving is the document as it is now, and a retired one
    // isn't. Retired ones found by this walk don't include those retired just now.
    done.extend(bring_back(notebook, &walked.set_aside, &logs, &is_open));
    done
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
    Retire { opened_there: bool },
    Move { home: PathBuf },
}

/// What's to be done with a stranded database, or `None` to leave it alone: it has more than one
/// home, its own logs have turned up since it was looked at, or what's at its home can't be read.
fn what_to_do(path: &Path, revision: &str, logs: &[Logs]) -> Option<Verdict> {
    let homes = homes_of(revision, logs);
    let home = match homes[..] {
        [] => {
            return Some(Verdict::Retire {
                opened_there: false,
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
                Some(Verdict::Retire { opened_there: true })
            }
        }
        _ => None,
    }
}

/// A stranded database that has a home to go to.
struct Mover {
    path: PathBuf,
    home: PathBuf,
}

/// Those that can go, which isn't all of them. A mover needs its home empty, or holding only
/// another mover, which then takes everything there along with it. Two wanting one home can't both
/// have it, and which it should be isn't known. Dropping a mover can leave another without a home
/// to go to, so this repeats until none drop. A swap stays in, since each makes room for the other.
fn those_that_can_go(mut movers: Vec<Mover>, is_open: &impl Fn(&Path) -> bool) -> Vec<Mover> {
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
fn carry_out_moves(movers: Vec<Mover>, is_open: &impl Fn(&Path) -> bool) -> Vec<Done> {
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

/// Renames retired databases to join logs that hold their revision, at a path with no database.
/// A database with the same home that's still stranded, waiting its turn or on the screen, is
/// further on than the retired one, so that home is its to take. Each of several retired ones
/// wanting one home stays as it is, as with movers.
fn bring_back(
    notebook: &Notebook,
    set_aside: &[PathBuf],
    logs: &[Logs],
    is_open: &impl Fn(&Path) -> bool,
) -> Vec<Done> {
    let mut wanting: Vec<(&PathBuf, &Path)> = Vec::new();
    for path in set_aside.iter().filter(|path| !is_open(path)) {
        let Some(revision) = recorded_revision(path) else {
            continue;
        };
        if let [home] = homes_of(&revision, logs)[..] {
            wanting.push((path, home));
        }
    }
    let alone = |home: &Path| wanting.iter().filter(|(_, other)| *other == home).count() == 1;
    let mut coming_back: Vec<(&PathBuf, &Path)> = wanting
        .iter()
        .copied()
        .filter(|(_, home)| alone(home) && !is_open(home) && is_free(home))
        .collect();
    if coming_back.is_empty() {
        return Vec::new();
    }
    let spoken_for: Vec<String> = find_stranded(notebook)
        .iter()
        .filter_map(|path| recorded_revision(path))
        .collect();
    coming_back.retain(|(_, home)| {
        !spoken_for
            .iter()
            .any(|revision| homes_of(revision, logs).contains(home))
    });
    coming_back.sort();
    let mut done = Vec::new();
    for (path, home) in coming_back {
        if arrive(path, home) {
            done.push(Done::Returned {
                from: path.clone(),
                to: home.to_path_buf(),
            });
        }
    }
    done
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
