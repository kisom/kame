//! Merging two `.kame` files, such as a document and a Syncthing conflict copy beside it.

use std::path::Path;

use kame_core::edit::apply_inverse;
use kame_core::edit::Op;
use kame_core::model::Document;
use kame_core::storage::Db;

use crate::merge::{merge, Merged, Side};
use kame_core::session::Session;
use std::path::PathBuf;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A document as a file holds it, with the part of its log that led to it.
pub struct Version {
    pub doc: Document,
    pub ops: Vec<Op>,
}

impl Version {
    pub fn read(path: &Path) -> Result<Version> {
        let db = Db::open_read_only(path)?;
        let doc = db.load_document()?;
        let cursor = db.cursor()?;
        let mut ops: Vec<Op> = db.load_ops()?.into_iter().map(|(_, op)| op).collect();
        // What undo took back is not part of how this document came to be.
        ops.truncate(cursor);
        Ok(Version { doc, ops })
    }
}

/// How many ops the two share from the start.
pub fn shared_ops(one: &[Op], other: &[Op]) -> usize {
    one.iter().zip(other).take_while(|(a, b)| a == b).count()
}

/// The document as it was after the ops both share, found by undoing the rest of one side.
///
/// The ops are only a way to get there. If compacting one side's history has left nothing shared,
/// this gives an older document than the true one, which means more conflicts and no lost words.
pub fn shared_base(version: &Version, shared: usize) -> Document {
    let mut doc = version.doc.clone();
    for op in version.ops[shared..].iter().rev() {
        apply_inverse(&mut doc, op);
    }
    doc
}

/// As [`merge_versions`], with `one` being what is on the screen: its words are the ones left in
/// the text when a sentence conflicts, so a merge never rewrites what the author is looking at.
fn merge_into_screen(one: &Version, other: &Version, other_label: &str) -> Merged {
    let base = shared_base(one, shared_ops(&one.ops, &other.ops));
    merge(
        Some(&base),
        Side {
            doc: &one.doc,
            label: "this machine",
            when: 1,
        },
        Side {
            doc: &other.doc,
            label: other_label,
            when: 0,
        },
    )
}

pub fn merge_versions(
    one: &Version,
    one_label: &str,
    other: &Version,
    other_label: &str,
) -> Merged {
    let shared = shared_ops(&one.ops, &other.ops);
    let base = shared_base(one, shared);
    // A file does not say when it was written, so the longer history counts as the later.
    merge(
        Some(&base),
        Side {
            doc: &one.doc,
            label: one_label,
            when: one.ops.len() as i64,
        },
        Side {
            doc: &other.doc,
            label: other_label,
            when: other.ops.len() as i64,
        },
    )
}

/// The part of a sync conflict file's name after the document's, as Syncthing writes it.
const CONFLICT_MARK: &str = ".sync-conflict-";

/// Files beside `path` that a sync tool kept as other versions of this document. Others can be
/// named that way, so what counts is that they are the same document, by id.
pub fn conflict_copies(path: &Path) -> Vec<PathBuf> {
    let (Some(stem), Some(folder)) = (path.file_stem(), path.parent()) else {
        return Vec::new();
    };
    let folder = if folder.as_os_str().is_empty() {
        Path::new(".")
    } else {
        folder
    };
    let prefix = format!("{}{CONFLICT_MARK}", stem.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut copies: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|found| {
            found.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                name.starts_with(&prefix) && name.ends_with(".kame")
            })
        })
        .collect();
    // The app asks every few seconds, and there is rarely anything to open.
    if copies.is_empty() {
        return copies;
    }
    let Ok(id) = Db::open_read_only(path)
        .and_then(|db| db.load_document())
        .map(|doc| doc.id)
    else {
        return Vec::new();
    };
    copies.retain(|found| same_document(found, id));
    copies.sort();
    copies
}

fn same_document(path: &Path, id: uuid::Uuid) -> bool {
    Db::open_read_only(path)
        .and_then(|db| db.load_document())
        .is_ok_and(|doc| doc.id == id)
}

/// What to call a copy in a note: when it dates from, if its name says.
fn copy_label(copy: &Path) -> String {
    let name = copy
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stamp = name.split(CONFLICT_MARK).nth(1).and_then(|rest| {
        let date = rest.get(..8)?;
        let time = rest.get(9..13)?;
        let digits = |text: &str| text.bytes().all(|byte| byte.is_ascii_digit());
        (digits(date) && digits(time)).then(|| {
            format!(
                "{}-{}-{} {}:{}",
                &date[..4],
                &date[4..6],
                &date[6..8],
                &time[..2],
                &time[2..]
            )
        })
    });
    match stamp {
        Some(stamp) => format!("the copy from {stamp}"),
        None => "the other copy".to_string(),
    }
}

/// What an open session holds, including anything not yet saved.
fn version_of(session: &Session) -> Version {
    Version {
        doc: session.document().clone(),
        ops: session.editor.applied_ops().to_vec(),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct MergedIn {
    /// Notes the merge left for the author.
    pub notes: usize,
    /// Whether the document changed at all.
    pub changed: bool,
}

/// Merges `copy` into an open document as one undo step, with a snapshot before it, then renames
/// the copy so that it is no longer offered. It stays there under that name, since undoing the
/// merge should not leave it the only place those words are.
pub fn merge_copy_into(session: &mut Session, copy: &Path) -> Result<MergedIn> {
    if session.save_failed {
        return Err("this document cannot be saved at the moment".into());
    }
    let other = Version::read(copy)?;
    if other.doc.id != session.document().id {
        return Err("that is a different document".into());
    }
    let ours = version_of(session);
    let merged = merge_into_screen(&ours, &other, &copy_label(copy));
    let changed = merged.doc != ours.doc;
    if changed {
        if session
            .snapshot(Some("before merging a conflict copy"))
            .is_none()
        {
            return Err("could not take a snapshot first, so nothing was merged".into());
        }
        if !session.replace_document(merged.doc) {
            return Err("could not save the merge".into());
        }
    }
    // Words and accepted marks live beside the text, and neither copy's should be lost.
    if let Some(theirs) = Db::open_read_only(copy)?.load_metadata()? {
        let mut metadata = session.metadata.clone();
        metadata.take_exceptions_from(&theirs);
        if metadata != session.metadata {
            session.set_metadata(metadata);
        }
    }
    let mut retired = copy.as_os_str().to_owned();
    retired.push(".merged");
    std::fs::rename(copy, retired)?;
    Ok(MergedIn {
        notes: merged.notes.len(),
        changed,
    })
}

/// Merges what an open session holds with the file that replaced its own, returning a session on
/// that file with the merge as its last undo step. `old` still has the words if this fails, and
/// it is for the caller to drop it.
pub fn merge_replaced(old: &Session) -> Result<(Session, MergedIn)> {
    let path = old.path().ok_or("this document has no file")?.to_path_buf();
    // Opening a missing path would create a document there.
    if !path.is_file() {
        return Err("that file is no longer there".into());
    }
    let arrived = Version::read(&path)?;
    if arrived.doc.id != old.document().id {
        return Err("the file there now is a different document".into());
    }
    let ours = version_of(old);
    let merged = merge_into_screen(&ours, &arrived, "the other machine");
    let changed = merged.doc != arrived.doc;
    let mut session = Session::open(&path).map_err(|err| err.to_string())?;
    if changed {
        if session
            .snapshot(Some("before merging with the copy on this machine"))
            .is_none()
        {
            return Err("could not take a snapshot first, so nothing was merged".into());
        }
        if !session.replace_document(merged.doc) {
            return Err("could not save the merge".into());
        }
    }
    let mut metadata = session.metadata.clone();
    metadata.take_exceptions_from(&old.metadata);
    if metadata != session.metadata {
        session.set_metadata(metadata);
    }
    Ok((
        session,
        MergedIn {
            notes: merged.notes.len(),
            changed,
        },
    ))
}
