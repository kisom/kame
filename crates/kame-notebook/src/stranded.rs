//! Databases left behind on this machine after their document was moved or deleted on another. In
//! a folder with revision logs, Syncthing carries the logs and not the databases, so a database
//! finds out only by its logs having gone. See "The other machines" in
//! `docs/superpowers/specs/2026-11-04-notebook-file-operations-design.md`.
//!
//! This runs with nobody looking, so all it does to a database is rename it, never over anything, and
//! write down where it is. It doesn't retire a database as having no home if some logs that might be
//! its home can't be read, and says so. What it doesn't settle on one pass, it sees again on the next.
//!
//! It comes in two parts, since reading every log is slow, and a rename mustn't be on another thread
//! from whatever opens documents. [`decide`] only reads, and can be on any thread. [`apply`] does
//! the renames, on the thread that opens documents: `build_from_logs` puts a database in place over
//! whatever has got there since it looked, so a rename landing there would be lost, and a document
//! opened during one would be moved out from under its session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kame_core::lock::Locks;
use kame_core::model::Metadata;
use kame_core::storage::Db;
use kame_sync::logs::{
    is_set_aside, is_where_it_synced, log_folder, logging_root, place_of, stranded_verdict,
};
use kame_sync::merge::document_digest;
use kame_sync::revlog::Store;

use crate::list::{NOT_DOCUMENTS, OUTPUT};
use crate::manage::{is_database, is_on_others_logs_database, is_stranded_database, sqlite_files};
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
    stamping: Vec<(PathBuf, LastSync)>,
    unsettled: Option<Unsettled>,
}

#[derive(Debug)]
struct Retiring {
    path: PathBuf,
    last: LastSync,
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

/// A database with a home to go to, and what it remembered when that was worked out.
#[derive(Debug)]
struct Going {
    path: PathBuf,
    last: LastSync,
    home: PathBuf,
}

/// What a database remembers of its last sync: the revision, and where it was.
#[derive(Clone, Debug, PartialEq)]
struct LastSync {
    revision: String,
    place: Option<String>,
}

/// Why databases that have waited can't be settled, for saying so.
#[derive(Clone, Debug, PartialEq)]
pub struct Unsettled {
    /// How many.
    pub waiting: usize,
    /// From the notebook's root, the first thing in the way.
    pub because: String,
}

impl Steps {
    /// What has waited long enough to be settled and can't be, if anything.
    pub fn unsettled(&self) -> Option<&Unsettled> {
        self.unsettled.as_ref()
    }
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

/// What a database's healthy verdict rests on: what it remembers of its last sync, and each log's
/// name and size. Logs are only ever added to, so this comes from looking at them, with no reading.
#[derive(Debug, PartialEq)]
struct LooksHealthy {
    last: LastSync,
    logs: Vec<(PathBuf, u64)>,
}

/// The databases in the notebook that have lost their logs, for now at least. A database's logs are
/// read only if it has no verdict in `healthy` that still holds. So that's only one that's stranded,
/// or has logs, a revision or a place new since the last look.
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
        let Some(last) = last_sync(&path) else {
            continue;
        };
        let (revision, place) = (last.revision.clone(), last.place.clone());
        // Before any reading, so that logs added to meanwhile are read again next time.
        let looks = Store::log_sizes(&log_folder(&path)).map(|logs| LooksHealthy { last, logs });
        if looks.is_some() && looks.as_ref() == known.get(&path) {
            healthy.databases.extend(looks.map(|looks| (path, looks)));
            continue;
        }
        match stranded_verdict(&path, Some(&revision), place.as_deref()) {
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
///
/// Having found a home settles a move, a retirement because the document's already been opened
/// there, and a return, whatever else can't be read. Only a retirement for having no home needs all
/// of them, and what stops one is said in [`Steps::unsettled`].
pub fn decide(notebook: &Notebook, may_act_on: &[PathBuf]) -> Steps {
    let mut steps = Steps {
        root: notebook.root.clone(),
        retiring: Vec::new(),
        moving: Vec::new(),
        returning: Vec::new(),
        stamping: those_to_stamp(notebook),
        unsettled: None,
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
    let mut logs = log_folders(&walked.documents);
    logs.cannot_read.append(&mut walked.unreadable);
    logs.cannot_read.sort();

    let mut waiting = 0;
    let mut because = None;
    for path in to_settle {
        let Some(last) = last_sync(&path) else {
            continue;
        };
        // It may hold more than its logs, and this build can't tell, so it's left as it is for a
        // kame that can.
        if holds_newer(&path) {
            continue;
        }
        let contents = contents_of(&path);
        match what_to_do(&path, &last.revision, contents.as_ref(), &logs) {
            Some(Verdict::Retire {
                opened_there,
                unsynced,
            }) => steps.retiring.push(Retiring {
                path,
                last,
                contents,
                opened_there,
                unsynced,
            }),
            Some(Verdict::Move { home }) => steps.moving.push(Going { path, last, home }),
            Some(Verdict::CantRuleOutAHome { because: this }) => {
                waiting += 1;
                because.get_or_insert(this);
            }
            None => {}
        }
    }
    steps.unsettled = because.map(|because| Unsettled {
        waiting,
        because: relative_to(&notebook.root, &because),
    });
    steps.returning = those_coming_back(notebook, &walked.set_aside, &logs.read);
    steps
}

/// Does what was found, on the thread that opens documents, so that nothing can be built or opened
/// in the middle of a rename. `open` is what's open now, not when `decide` began. Time has passed
/// since then, so each step is looked at again just before it's taken, with no more reading than
/// that takes: it's skipped if the database has gone, is open, or remembers something else now, or
/// if something has arrived where it's going. A retirement is also skipped if what the database
/// holds has changed, since what it reports about that would be out of date. A database another kame
/// has open, as `locks` tells, is skipped too, and the lock on each one acted on is held while it's
/// renamed or written. This returns what was done, and what's skipped or fails is left to be seen
/// again by the next pass.
pub fn apply(
    notebook: &Notebook,
    steps: Steps,
    open: Option<&Path>,
    locks: Locks<'_>,
) -> Vec<Done> {
    if steps.root != notebook.root {
        return Vec::new();
    }
    let is_open = |path: &Path| open.is_some_and(|open| same_file(open, path));
    let as_it_was =
        |path: &Path, last: &LastSync| !is_open(path) && last_sync(path).as_ref() == Some(last);

    let mut done = Vec::new();
    for step in steps.retiring {
        let Ok(_holding) = locks.take([&step.path]) else {
            continue;
        };
        if !as_it_was(&step.path, &step.last) || contents_of(&step.path) != step.contents {
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
    // After the retirements, which may have freed a name that a mover is going to. One that stays
    // where it is leaves no room for another.
    let (mut movers, mut holding) = (Vec::new(), Vec::new());
    for mover in steps.moving {
        let Ok(held) = locks.take([&mover.path]) else {
            continue;
        };
        if as_it_was(&mover.path, &mover.last) {
            movers.push(mover);
            holding.push(held);
        }
    }
    done.extend(carry_out_moves(movers, &is_open));
    drop(holding);
    // After the moves, since a database moving is the document as it is now, and a retired one
    // isn't.
    for step in steps.returning {
        let Ok(_holding) = locks.take([&step.path]) else {
            continue;
        };
        if as_it_was(&step.path, &step.last)
            && !is_open(&step.home)
            && arrive(&step.path, &step.home)
        {
            record_place(&step.home);
            done.push(Done::Returned {
                from: step.path,
                to: step.home,
            });
        }
    }
    for (path, last) in steps.stamping {
        let Ok(_holding) = locks.take([&path]) else {
            continue;
        };
        if as_it_was(&path, &last) {
            record_place(&path);
        }
    }
    done
}

/// Both, for callers with nothing else going on.
pub fn reconcile(
    notebook: &Notebook,
    may_act_on: &[PathBuf],
    open: Option<&Path>,
    locks: Locks<'_>,
) -> Vec<Done> {
    apply(notebook, decide(notebook, may_act_on), open, locks)
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

/// What a database remembers of its last sync, if it has synced.
fn last_sync(path: &Path) -> Option<LastSync> {
    let db = Db::open_read_only(path).ok()?;
    Some(LastSync {
        revision: db.load_sync_state()?,
        place: db.load_sync_place(),
    })
}

/// Databases from before places were recorded, with their revision in the logs beside them, which
/// shows they synced where they are, so that they can say so. A copy made by hand, with no logs beside
/// it, doesn't show that. Once one has a place it isn't asked about again, so this reads logs for a
/// database only until then, and on every pass only for one whose revision isn't beside it. One that
/// can't take a place yet is passed over before any of that, and is just asked again next time.
fn those_to_stamp(notebook: &Notebook) -> Vec<(PathBuf, LastSync)> {
    notebook
        .documents()
        .into_iter()
        .map(|entry| entry.path)
        .filter(|path| is_database(path) && path.is_file() && log_folder(path).is_dir())
        .filter(|path| logging_root(path).is_some() && can_record_place(path))
        .filter_map(|path| {
            let last = last_sync(&path)?;
            let shows_it = last.place.is_none()
                && Store::load(&log_folder(&path))
                    .is_ok_and(|store| store.could_hold(&last.revision));
            shows_it.then_some((path, last))
        })
        .collect()
}

/// Whether a database can have its place written with nothing else happening to it. Opening it for
/// writing folds in any `-wal` beside it, which may be another machine's, carried here by a sync tool,
/// and it may be open in another process. So it waits until nothing is beside it. `Db::open` also
/// makes a file that's gone, and leaves side files beside one it can't write.
fn can_record_place(path: &Path) -> bool {
    std::fs::OpenOptions::new().write(true).open(path).is_ok()
        && sqlite_files(path).iter().all(|side| {
            matches!(side.symlink_metadata(),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound)
        })
}

/// Says where a database is, so that it's recognised if its logs move on before it's next opened, if
/// that can be done with nothing else happening to it. Otherwise, or if this fails, it's left saying
/// somewhere else or nothing, and is merely not stranded. One holding something from a newer version
/// of kame is left as it is, since nothing this build does writes to that.
fn record_place(path: &Path) {
    if !can_record_place(path) || Db::open_read_only(path).map_or(true, |db| db.holds_newer()) {
        return;
    }
    if let (Some(place), Ok(mut db)) = (place_of(path), Db::open(path)) {
        let _ = db.save_sync_place(&place);
    }
}

/// What a walk of the notebook found.
#[derive(Default)]
struct Walked {
    /// The document each log folder stands for.
    documents: Vec<PathBuf>,
    /// Retired databases, and any left in a temporary name by a crash.
    set_aside: Vec<PathBuf>,
    /// The folders it should have seen and couldn't read.
    unreadable: Vec<PathBuf>,
}

/// Goes through the folders `Notebook::entries` finds documents in, the same way: symbolic links
/// aren't followed, and hidden folders are passed over, which is also what keeps the old logs in a
/// sync tool's `.stversions` from being taken for a home. Another notebook's folder is passed over
/// too, so its logs aren't a home here and nothing set aside in it is brought back.
fn walk(notebook: &Notebook, folder: &Path, walked: &mut Walked) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        walked.unreadable.push(folder.to_path_buf());
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            walked.unreadable.push(folder.to_path_buf());
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            walked.unreadable.push(folder.to_path_buf());
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
            if !name.starts_with('.') && !no_documents && !crate::is_notebook_root(&entry.path()) {
                walk(notebook, &entry.path(), walked);
            }
        } else if kind.is_file() && is_set_aside(&name) {
            walked.set_aside.push(entry.path());
        }
    }
}

/// One log folder, read.
struct Logs {
    document: PathBuf,
    store: Store,
}

/// What reading every log folder found.
struct AllLogs {
    read: Vec<Logs>,
    /// What can't be read, or holds lines from a newer kame that were skipped, and could be holding
    /// any revision.
    cannot_read: Vec<PathBuf>,
}

/// The logs of each of these documents.
fn log_folders(documents: &[PathBuf]) -> AllLogs {
    let mut all = AllLogs {
        read: Vec::new(),
        cannot_read: Vec::new(),
    };
    for document in documents {
        let folder = log_folder(document);
        let store = std::fs::read_dir(&folder)
            .ok()
            .and_then(|_| Store::load(&folder).ok());
        match store {
            Some(store) => {
                if store.holds_newer() {
                    all.cannot_read.push(document.clone());
                }
                all.read.push(Logs {
                    document: document.clone(),
                    store,
                });
            }
            None => all.cannot_read.push(document.clone()),
        }
    }
    all.read
        .sort_by(|one, other| one.document.cmp(&other.document));
    all
}

/// The documents whose logs hold a revision, as far as their lines say.
fn homes_of<'a>(revision: &str, logs: &'a [Logs]) -> Vec<&'a Path> {
    logs.iter()
        .filter(|logs| logs.store.could_hold(revision))
        .map(|logs| logs.document.as_path())
        .collect()
}

/// What could be hiding a home for this database: anything that can't be read, or logs with a corrupt
/// line that could be of its document. A database whose document can't be read could be of any.
///
/// Logs whose corrupt line would only change why it's retired don't count: were its revision there,
/// it would be retired against a database of its document already opened at them, which is the same
/// rename, and says no more than that it may hold what no log does. A line hiding it never makes it
/// usable, so that's said either way.
fn what_could_hide_its_home(
    path: &Path,
    revision: &str,
    contents: Option<&Contents>,
    logs: &AllLogs,
) -> Option<PathBuf> {
    if let Some(first) = logs.cannot_read.first() {
        return Some(first.clone());
    }
    // One from a newer version doesn't get this far: `decide` leaves it alone.
    let document = Db::open_read_only(path)
        .and_then(|db| db.load_document())
        .ok()
        .map(|document| document.id);
    logs.read
        .iter()
        .filter(|logs| match document {
            Some(document) => logs.store.may_hide(document),
            None => logs.store.corrupt_lines(),
        })
        // Its own logs holding it would mean it isn't stranded, which isn't a retirement either.
        .find(|hiding| {
            hiding.document == path
                || !matches!(
                    at_home(&hiding.document, revision, contents, &logs.read),
                    Some(Verdict::Retire { .. })
                )
        })
        .map(|logs| logs.document.clone())
}

fn relative_to(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(inside) if !inside.as_os_str().is_empty() => inside.display().to_string(),
        _ => path.display().to_string(),
    }
}

enum Verdict {
    Retire { opened_there: bool, unsynced: bool },
    Move { home: PathBuf },
    CantRuleOutAHome { because: PathBuf },
}

/// What's to be done with a stranded database, or `None` to leave it alone: it has more than one
/// home, its own logs have turned up since it was looked at, or what's at its home can't be read.
fn what_to_do(
    path: &Path,
    revision: &str,
    contents: Option<&Contents>,
    all: &AllLogs,
) -> Option<Verdict> {
    let logs = &all.read[..];
    let homes = homes_of(revision, logs);
    let home = match homes[..] {
        [] => {
            // Having no home has to be sure, or a retirement would be a guess.
            return Some(
                match what_could_hide_its_home(path, revision, contents, all) {
                    Some(because) => Verdict::CantRuleOutAHome { because },
                    None => Verdict::Retire {
                        opened_there: false,
                        unsynced: true,
                    },
                },
            );
        }
        [home] if home != path => home,
        _ => return None,
    };
    at_home(home, revision, contents, logs)
}

/// What's to be done with a stranded database whose revision is in the logs at `home`, another
/// path than its own: moved there if nothing's there, or a database that's leaving too is, retired
/// if its document has been opened there, and otherwise left alone.
fn at_home(
    home: &Path,
    revision: &str,
    contents: Option<&Contents>,
    logs: &[Logs],
) -> Option<Verdict> {
    let moves_in = Verdict::Move {
        home: home.to_path_buf(),
    };
    match home.symlink_metadata() {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(moves_in),
        Ok(metadata) if metadata.is_file() && Db::open_read_only(home).is_ok() => {
            if is_stranded_database(home) {
                // It may be leaving too, and if it isn't, this waits for it.
                Some(moves_in)
            } else if is_on_others_logs_database(home) {
                // A copy of something else, put there by hand, doesn't mean this was opened there.
                None
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

/// Whether a database holds something from a newer version of kame: a field, kind, style or other
/// value this build doesn't know, or a step of history it can't read.
fn holds_newer(path: &Path) -> bool {
    Db::open_read_only(path).is_ok_and(|db| db.holds_newer())
}

/// What a database holds, if that can be read. One holding anything from a newer version of kame
/// can't be, a field as much as a kind: what this build reads of it leaves that out, so it could
/// match the logs while the file holds more.
fn contents_of(path: &Path) -> Option<Contents> {
    let db = Db::open_read_only(path).ok()?;
    if db.holds_newer() {
        return None;
    }
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
            record_place(&mover.home);
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
        let Some(last) = last_sync(path) else {
            continue;
        };
        if let [home] = homes_of(&last.revision, logs)[..] {
            let home = home.to_path_buf();
            wanting.push(Going {
                path: path.clone(),
                last,
                home,
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

/// What each stranded database remembers, going by logs that have already been read. A database
/// with no logs among them has none that hold its revision, as far as this can tell, and it keeps the
/// home for itself, which at worst leaves a retired one waiting.
fn revisions_of_stranded(notebook: &Notebook, logs: &[Logs]) -> Vec<String> {
    notebook
        .documents()
        .into_iter()
        .map(|entry| entry.path)
        .filter(|path| is_database(path) && path.is_file() && logging_root(path).is_some())
        .filter_map(|path| {
            let last = last_sync(&path)?;
            if !is_where_it_synced(&path, last.place.as_deref()) {
                return None;
            }
            let own = logs.iter().find(|logs| logs.document == path);
            (!own.is_some_and(|logs| logs.store.could_hold(&last.revision)))
                .then_some(last.revision)
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

    /// `Db::open` would make a blank one, if a delete from another process landed just before it.
    #[test]
    fn a_place_is_not_recorded_where_the_file_has_gone() {
        let root = kame_test_dir::folder("stranded-record-gone");
        std::fs::write(root.join(kame_sync::logs::MARKER), "").unwrap();
        let gone = root.join("gone.kame");
        record_place(&gone);
        assert!(!gone.exists());
    }

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
}
