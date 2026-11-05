//! Changing what's in a notebook: where a document may be put, deleting one, and moving one with
//! the links that lead to it.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use kame_core::links::{moved_target, own_target_after_move};
use kame_core::model::{BlockKind, Document, SpanStyle};
use kame_core::session::{is_markdown_path, Session};
use kame_core::storage::Db;
use kame_sync::logs::{
    build_from_logs, is_stranded, log_folder, logging_root, logs_can_be_its_own, Logged,
};
use kame_sync::revlog::Store;

use crate::links::{is_relative, tidied, Follow};
use crate::search::same_file;
use crate::{read_document, Notebook};

/// Where `typed` puts `document`, or why it can't go there. The path is taken from the notebook's
/// root, and gets the document's extension if it names none. Nothing is changed on disk.
pub fn check_new_path(
    notebook: &Notebook,
    document: &Path,
    typed: &str,
) -> Result<PathBuf, String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Err("type a path for it".to_string());
    }
    if typed.starts_with('/') || typed.starts_with('~') {
        return Err("a path starts in the notebook, so it can't begin with / or ~".to_string());
    }
    if typed.ends_with('/') {
        return Err("that names a folder; end it with the document's name".to_string());
    }
    let mut path = tidied(&notebook.root.join(typed));
    if path == notebook.root || !path.starts_with(&notebook.root) {
        return Err("that leads outside the notebook".to_string());
    }
    let extension = document
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned())
        .unwrap_or_default();
    match path.extension() {
        None => {
            let mut named = path.into_os_string();
            named.push(".");
            named.push(&extension);
            path = PathBuf::from(named);
        }
        Some(given) => {
            if !given.to_string_lossy().eq_ignore_ascii_case(&extension) {
                return Err(format!("it has to stay a .{extension} document"));
            }
        }
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.contains(".sync-conflict-") {
        return Err("that name is the sync tool's, for a conflicting copy".to_string());
    }
    if !notebook.holds(&path) {
        return Err("that leads outside the notebook".to_string());
    }
    if !notebook.in_documents_area(&path) {
        return Err(
            "documents can't go in a hidden folder, or in static, images, theme or output"
                .to_string(),
        );
    }
    let taken = path.symlink_metadata().is_ok() || log_folder(&path).exists();
    if taken && !same_file(&path, document) {
        let relative = path.strip_prefix(&notebook.root).unwrap_or(&path);
        return Err(format!(
            "something is already at {}; delete it first",
            relative.display()
        ));
    }
    Ok(path)
}

/// The files kept for a document beside it: conflicting copies from a sync tool, each with the
/// copy a merge left of it, and databases retired from it. Names are matched whole, so a document
/// `XY` isn't taken for `X`. A retired copy is also matched by id, as a conflicting copy is, since
/// a path can be reused, and `.X.retired.kame` may be the only copy of the old `X`'s words.
pub fn belonging_files(path: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for copy in kame_sync::files::conflict_copies(path) {
        let mut merged = copy.clone().into_os_string();
        merged.push(".merged");
        let merged = PathBuf::from(merged);
        found.push(copy);
        if merged.symlink_metadata().is_ok() {
            found.push(merged);
        }
    }
    let (Some(stem), Some(folder)) = (path.file_stem(), path.parent()) else {
        return found;
    };
    let before = format!(".{}.retired", stem.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(folder) else {
        return found;
    };
    let mut retired: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|entry| {
            let name = entry
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let Some(rest) = name
                .strip_prefix(&before)
                .and_then(|rest| rest.strip_suffix(".kame"))
            else {
                return false;
            };
            rest.is_empty()
                || rest
                    .strip_prefix('-')
                    .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
        .collect();
    retired.sort();
    found.extend(kame_sync::files::copies_of(path, retired));
    found
}

pub(crate) fn is_database(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("kame"))
}

/// What SQLite keeps beside a database.
pub(crate) fn sqlite_files(path: &Path) -> [PathBuf; 2] {
    ["-wal", "-shm"].map(|suffix| {
        let mut beside = path.as_os_str().to_owned();
        beside.push(suffix);
        PathBuf::from(beside)
    })
}

/// Deletes a document, and with it what belongs to it: what SQLite keeps beside the file, its
/// conflicting and retired copies, by id, and its logs. A file that can't be removed is an error before
/// the logs are touched, so that a document left in place keeps them. A stranded database has no
/// logs of its own, and one whose logs can't be shown to be its own may not: what's at its log
/// folder stays, as removing it could remove another document on every machine. This returns whether
/// it left logs there, since the document at that path still lists, and a second delete would take
/// them.
pub fn delete_document(path: &Path) -> std::io::Result<bool> {
    if !is_database(path) {
        std::fs::remove_file(path)?;
        return Ok(false);
    }
    // Found first, since which copies are this document's is told by reading it.
    let belonging = belonging_files(path);
    // Before the file goes, since it's the file that says.
    let its_own = logs_are_its_own(path);
    // A document this machine hasn't opened yet has only its logs.
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound || !log_folder(path).is_dir() => {
            return Err(err);
        }
        _ => {}
    }
    for file in sqlite_files(path).into_iter().chain(belonging) {
        match std::fs::remove_file(&file) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
    }
    let logs = log_folder(path);
    if !logs.is_dir() {
        return Ok(false);
    }
    if its_own {
        std::fs::remove_dir_all(&logs)?;
    }
    Ok(!its_own)
}

/// A document a move couldn't change, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Left {
    pub path: PathBuf,
    pub relative: String,
    pub why: String,
}

/// What moving a document will change, worked out before anything has moved.
#[derive(Debug)]
pub struct MovePlan {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Each document with links to change: its old target string to its new one.
    rewrites: Vec<(PathBuf, HashMap<String, String>)>,
    left: Vec<Left>,
}

/// What a move did: the documents whose links changed, and the ones that couldn't be changed.
#[derive(Debug, Default)]
pub struct MoveReport {
    pub changed: Vec<String>,
    pub left: Vec<Left>,
}

const NEWER: &str = "it has changes from a newer version of kame";
const OTHERS_LOGS: &str =
    "the logs at its path may be another document's, so it isn't syncing. Save As gives it logs of \
     its own";

/// Every link in a document: web links and document links in its text, and an image's link.
fn links_in(document: &Document) -> Vec<&str> {
    let mut links = Vec::new();
    for block in &document.blocks {
        for span in &block.spans {
            match &span.style {
                SpanStyle::Link { url } => links.push(url.as_str()),
                SpanStyle::DocumentLink { target } => links.push(target.as_str()),
                _ => {}
            }
        }
        if let BlockKind::Image {
            link: Some(link), ..
        } = &block.kind
        {
            links.push(link.as_str());
        }
    }
    links
}

/// The links in `document`, kept at `base`, that lead to `file`.
fn links_leading_to(
    notebook: &Notebook,
    document: &Document,
    base: &Path,
    file: &Path,
) -> Vec<String> {
    links_in(document)
        .into_iter()
        .filter(|link| match notebook.follow(link, base) {
            Follow::Open(found) => same_file(&found, file),
            _ => false,
        })
        .map(str::to_string)
        .collect()
}

/// Whether the database at `path`, in a folder with logs, has lost them, as read from its file. One
/// with only logs or never synced has no state to lose, and one copied here was never at its logs.
/// Nothing above it turning logs on means its state says nothing, and nothing would ever settle it.
pub(crate) fn is_stranded_database(path: &Path) -> bool {
    let Ok(db) = Db::open_read_only(path) else {
        return false;
    };
    is_stranded(
        path,
        db.load_sync_state().as_deref(),
        db.load_sync_place().as_deref(),
    )
}

/// Whether a database with a history sits on logs that can't be shown to be its own, as read from
/// its file. A file that can't say whose it is can't show that either.
pub(crate) fn is_on_others_logs_database(path: &Path) -> bool {
    let Ok(db) = Db::open_read_only(path) else {
        return true;
    };
    let revision = db.load_sync_state();
    revision.is_some()
        && db.load_document().map_or(true, |document| {
            !logs_can_be_its_own(path, document.id, revision.as_deref())
        })
}

/// Whether what's at a database's log folder can be taken for its logs, to read, delete or move. With
/// no file, only logs, they're the document. A file that can't be read can't show they're its own.
fn logs_are_its_own(path: &Path) -> bool {
    if matches!(path.symlink_metadata(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound)
    {
        return true;
    }
    let read =
        Db::open_read_only(path).and_then(|db| Ok((db.load_document()?, db.load_sync_state())));
    let Ok((document, revision)) = read else {
        return !log_folder(path).exists();
    };
    !is_stranded_database(path) && logs_can_be_its_own(path, document.id, revision.as_deref())
}

/// The latest versions in a logged document's logs. There are none to read if it has no logs, or
/// if what's at its log folder isn't its logs, since those say nothing about this document.
pub(crate) fn versions_in_logs(path: &Path) -> Vec<Document> {
    if logging_root(path).is_none() {
        return Vec::new();
    }
    if !logs_are_its_own(path) {
        return Vec::new();
    }
    let Ok(store) = Store::load(&log_folder(path)) else {
        return Vec::new();
    };
    store
        .heads()
        .iter()
        .map(|head| store.document(head))
        .collect()
}

/// A closed document, ready to change: built from its logs if it has only those, and with those
/// logs if it's logged. This says no before anything has changed if a change couldn't reach them.
fn open_to_change(path: &Path, device: &str) -> Result<(Session, Option<Logged>), String> {
    build_from_logs(path).map_err(|err| format!("could not build it from its logs: {err}"))?;
    if !path.exists() {
        return Err("it's no longer there".to_string());
    }
    let session = Session::open(path).map_err(|err| err.to_string())?;
    if session.newer {
        return Err(NEWER.to_string());
    }
    if logging_root(path).is_none() {
        return Ok((session, None));
    }
    let folder = log_folder(path);
    let store = Store::load(&folder).map_err(|err| format!("could not read its logs: {err}"))?;
    if store.holds_newer() {
        return Err(NEWER.to_string());
    }
    let (revision, place) = (session.sync_state(), session.sync_place());
    if is_stranded(path, revision.as_deref(), place.as_deref()) {
        return Err("it was moved or deleted on another machine, and isn't syncing".to_string());
    }
    if revision.is_some() && !logs_can_be_its_own(path, session.document().id, revision.as_deref())
    {
        return Err(OTHERS_LOGS.to_string());
    }
    let logged = Logged::open(&session, device).ok_or("could not read its logs")?;
    Ok((session, Some(logged)))
}

/// Changes a closed document, and sends the change to its logs if it has them. What's in the logs
/// comes in first, since if it were merged after the change, it would bring back what the change
/// took out, and be left unchanged itself.
pub(crate) fn edit_closed<T>(
    path: &Path,
    device: &str,
    now: i64,
    change: impl FnOnce(&mut Session) -> Result<T, String>,
) -> Result<T, String> {
    let (mut session, mut logged) = open_to_change(path, device)?;
    if let Some(logged) = &mut logged {
        logged
            .sync(&mut session, now)
            .map_err(|err| format!("could not sync it with its logs: {err}"))?;
    }
    let result = change(&mut session)?;
    if session.save_failed {
        return Err("could not save the change".to_string());
    }
    if let Some(logged) = &mut logged {
        logged
            .sync(&mut session, now)
            .map_err(|err| format!("it changed, but its logs couldn't be written: {err}"))?;
    }
    Ok(result)
}

/// The path from the notebook's root.
fn relative_in(notebook: &Notebook, path: &Path) -> String {
    path.strip_prefix(&notebook.root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn same_folder(one: &Path, other: &Path) -> bool {
    same_file(
        one.parent().unwrap_or(Path::new("")),
        other.parent().unwrap_or(Path::new("")),
    )
}

/// Why a Markdown document's links will be wrong after this move, if they will. It's imported with
/// no folder given, so that its images stay as written and the relative ones can be told.
fn why_markdown_is_left(
    notebook: &Notebook,
    path: &Path,
    moving: bool,
    from: &Path,
    to: &Path,
) -> Option<String> {
    let source = std::fs::read_to_string(path).ok()?;
    let stem = path.file_stem()?.to_string_lossy();
    let document =
        kame_core::import::markdown::from_markdown(&source, Path::new(""), &stem).document;
    let depends_on_its_folder = links_in(&document)
        .into_iter()
        .any(|link| is_relative(link) && !link.starts_with('/') && !link.starts_with('~'))
        || document.blocks.iter().any(|block| match &block.kind {
            BlockKind::Image { path, .. } => {
                !path.contains("://") && !Path::new(path).is_absolute()
            }
            _ => false,
        });
    if moving && !same_folder(from, to) && depends_on_its_folder {
        return Some(
            "it's Markdown, which kame doesn't edit, so its relative links weren't changed"
                .to_string(),
        );
    }
    (!links_leading_to(notebook, &document, path, from).is_empty()).then(|| {
        "it's Markdown, which kame doesn't edit, so its links to this document weren't changed"
            .to_string()
    })
}

/// Works out which links lead to `from` and what each becomes once the document is at `to`. Files
/// don't move yet. What this does change is `to`'s folders, which have to exist for the new links
/// to be worked out, and any closed document that links here and is behind its logs, which is
/// synced so that what gets rewritten is the whole of it. `open` is what's on the screen, read in
/// place of its file and left for its own session to sync.
pub fn plan_move(
    notebook: &Notebook,
    from: &Path,
    to: &Path,
    open: Option<(&Path, &Document)>,
    device: &str,
    now: i64,
) -> MovePlan {
    let mut plan = MovePlan {
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        rewrites: Vec::new(),
        left: Vec::new(),
    };
    // Going nowhere, a rewrite would only tidy how links are written.
    if from == to {
        return plan;
    }
    if let Some(folder) = to.parent() {
        // If this fails, so will moving the file, and that says why.
        let _ = std::fs::create_dir_all(folder);
    }
    for entry in notebook.documents() {
        let moving = same_file(&entry.path, from);
        if is_markdown_path(&entry.path) {
            if let Some(why) = why_markdown_is_left(notebook, &entry.path, moving, from, to) {
                let (path, relative) = if moving {
                    (to.to_path_buf(), relative_in(notebook, to))
                } else {
                    (entry.path.clone(), entry.relative.clone())
                };
                plan.left.push(Left {
                    path,
                    relative,
                    why,
                });
            }
            continue;
        }
        let on_screen = open
            .filter(|(path, _)| same_file(path, &entry.path))
            .map(|(_, document)| document);
        let read;
        let document = match on_screen {
            Some(document) => document,
            None => {
                let Some((document, _)) = read_document(&entry.path) else {
                    continue;
                };
                read = document;
                &read
            }
        };
        let mut targets: BTreeSet<String> = links_leading_to(notebook, document, &entry.path, from)
            .into_iter()
            .collect();
        let versions = versions_in_logs(&entry.path);
        for version in &versions {
            targets.extend(links_leading_to(notebook, version, &entry.path, from));
        }
        // A document to be rewritten comes together with its logs, and is planned from the result.
        // If that fails, the rewrite is left to fail for the same reason, and say so.
        let behind = versions.iter().any(|version| version != document);
        if !targets.is_empty()
            && behind
            && on_screen.is_none()
            && edit_closed(&entry.path, device, now, |_| Ok(())).is_ok()
        {
            if let Some((synced, _)) = read_document(&entry.path) {
                targets.extend(links_leading_to(notebook, &synced, &entry.path, from));
            }
        }
        let base_after = if moving { to } else { entry.path.as_path() };
        let rewrites: HashMap<String, String> = targets
            .into_iter()
            .map(|old| {
                let new = moved_target(&old, to, base_after);
                (old, new)
            })
            .collect();
        if moving {
            // All of them, even one staying as it is, since carrying out takes a link that
            // isn't here for one leading elsewhere, and says it again from the new folder.
            plan.rewrites.push((from.to_path_buf(), rewrites));
        } else if rewrites.iter().any(|(old, new)| old != new) {
            plan.rewrites.push((entry.path.clone(), rewrites));
        }
    }
    plan
}

/// Where a file belonging to the document at `from` goes when the document goes to `to`, with the
/// new name in place of the old.
fn belonging_file_after(file: &Path, from: &Path, to: &Path) -> PathBuf {
    let text = |name: Option<&std::ffi::OsStr>| {
        name.map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let (name, old, new) = (
        text(file.file_name()),
        text(from.file_stem()),
        text(to.file_stem()),
    );
    let renamed = match name.strip_prefix(&old) {
        Some(rest) => format!("{new}{rest}"),
        None => {
            let rest = name
                .strip_prefix('.')
                .and_then(|name| name.strip_prefix(&old))
                .unwrap_or_default();
            format!(".{new}{rest}")
        }
    };
    to.with_file_name(renamed)
}

/// A retired database going to `.X.retired.kame`, or a numbered one, takes the next number if
/// that's taken, since it's never put over another. Any other path is as it was.
fn free_retired_name(new: PathBuf, old: &Path) -> PathBuf {
    let name = new
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let is_free = |path: &Path| path.symlink_metadata().is_err() || same_file(path, old);
    let Some((before, _)) = name.rsplit_once(".retired") else {
        return new;
    };
    if !name.starts_with('.') || !name.ends_with(".kame") || is_free(&new) {
        return new;
    }
    (2..)
        .map(|number| new.with_file_name(format!("{before}.retired-{number}.kame")))
        .find(|path| is_free(path))
        .unwrap_or(new)
}

/// Moves the document's files, or leaves them all where they were and says why. Nothing goes over
/// anything already there. That's checked for every path this could use, whether or not there's
/// anything to go to it, as a `-wal` that a file now gone left behind would be read as the log of
/// the file arriving, and a log folder would be taken for its logs.
fn move_files(notebook: &Notebook, from: &Path, to: &Path) -> Result<(), String> {
    let mut moves = vec![(from.to_path_buf(), to.to_path_buf())];
    if is_database(from) {
        moves.extend(sqlite_files(from).into_iter().zip(sqlite_files(to)));
        moves.push((log_folder(from), log_folder(to)));
        for file in belonging_files(from) {
            let after = belonging_file_after(&file, from, to);
            moves.push((file, after));
        }
    }
    if moves.iter().all(|(old, _)| old.symlink_metadata().is_err()) {
        return Err(format!(
            "there's nothing at {}",
            relative_in(notebook, from)
        ));
    }
    for (old, new) in &moves {
        let new = free_retired_name(new.clone(), old);
        if new.symlink_metadata().is_ok() && !same_file(&new, old) {
            return Err(format!(
                "something is already at {}; delete it first",
                relative_in(notebook, &new)
            ));
        }
    }
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (old, new) in moves {
        if old.symlink_metadata().is_err() {
            continue;
        }
        let new = free_retired_name(new, &old);
        if let Err(err) = std::fs::rename(&old, &new) {
            let mut why = format!(
                "could not move {} to {}: {err}",
                relative_in(notebook, &old),
                relative_in(notebook, &new)
            );
            for (old, new) in moved.iter().rev() {
                if let Err(err) = std::fs::rename(new, old) {
                    why.push_str(&format!(
                        ", and {} couldn't go back: {err}",
                        relative_in(notebook, new)
                    ));
                }
            }
            return Err(why);
        }
        moved.push((old, new));
    }
    Ok(())
}

/// Moves a document and changes the links to it. If its files can't move, this is `Err`, with all
/// of them where they were and no document changed. Whatever can't be changed after that goes in
/// the report, and the others still change. `open` is the session on the screen, which mustn't
/// be on the moving document, since no connection may be open across the rename. A change to it
/// is saved here, and reaches its logs when the app next syncs it.
pub fn carry_out(
    notebook: &Notebook,
    plan: MovePlan,
    mut open: Option<&mut Session>,
    device: &str,
    now: i64,
) -> Result<MoveReport, String> {
    let MovePlan {
        from,
        to,
        rewrites,
        left,
    } = plan;
    let mut report = MoveReport {
        changed: Vec::new(),
        left,
    };
    if open
        .as_deref()
        .and_then(Session::path)
        .is_some_and(|path| same_file(path, &from))
    {
        return Err("a document has to be left before it moves".to_string());
    }
    if from == to {
        return Ok(report);
    }
    // What's at its log folder may be another document's by now, and that mustn't go with it.
    if is_database(&from) && is_stranded_database(&from) {
        return Err(
            "this document was moved or deleted on another machine, so it can't be moved until \
             that's settled"
                .to_string(),
        );
    }
    if is_database(&from) && !logs_are_its_own(&from) {
        return Err(OTHERS_LOGS.to_string());
    }
    move_files(notebook, &from, &to)?;

    let mut own = HashMap::new();
    for (path, links) in rewrites {
        if path == from {
            own = links;
            continue;
        }
        let rewrite =
            |session: &mut Session| session.rewrite_links(|link| links.get(link).cloned());
        let on_screen = open
            .as_deref_mut()
            .filter(|session| session.path().is_some_and(|open| same_file(open, &path)));
        let result = match on_screen {
            Some(session) => rewrite(session),
            None => edit_closed(&path, device, now, rewrite),
        };
        let relative = relative_in(notebook, &path);
        match result {
            Ok(0) => {}
            Ok(_) => report.changed.push(relative),
            Err(why) => report.left.push(Left {
                path,
                relative,
                why,
            }),
        }
    }

    if !is_database(&to) {
        return Ok(report);
    }
    let result = edit_closed(&to, device, now, |session| {
        let history = session.history();
        session.rewrite_links_by_kind(|link, document| {
            Some(
                own.get(link)
                    .cloned()
                    .unwrap_or_else(|| own_target_after_move(link, &from, &to, document)),
            )
        })?;
        if !same_folder(&from, &to) {
            session.rehome_images(Some(&from), Some(&notebook.root));
        }
        let changed = session.history() != history;
        if let (Some(old), Some(new)) = (from.file_stem(), to.file_stem()) {
            let (old, new) = (old.to_string_lossy(), new.to_string_lossy());
            if old != new {
                let follows = session.metadata.title == old;
                session.follow_rename(&old, &new);
                // The session has the new title even if saving it failed, so ask the file.
                let saved = Db::open_read_only(&to)
                    .ok()
                    .and_then(|db| db.load_metadata().ok().flatten());
                if follows && saved.is_none_or(|metadata| metadata.title != new) {
                    return Err(format!("its title couldn't be changed to {new}"));
                }
            }
        }
        Ok(changed)
    });
    let relative = relative_in(notebook, &to);
    match result {
        Ok(false) => {}
        Ok(true) => report.changed.push(relative),
        Err(why) => report.left.push(Left {
            path: to,
            relative,
            why,
        }),
    }
    Ok(report)
}
