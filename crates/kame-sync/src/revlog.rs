//! Revisions in logs that each have one writer, carried by anything that copies files.
//!
//! A shared database cannot survive file sync, since two machines write one file. Here every
//! machine appends only to its own log, so a sync tool never sees a conflict, and a log that
//! arrives late or partly written costs nothing. Revisions form a graph, as in git, and are
//! addressed by content. A merge's id depends only on what it merges and its result, so two
//! machines that merge separately write the same revision, and they have converged.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use kame_core::model::{Annotation, Block, Document, Metadata};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::digest;
use crate::merge::{document_digest, merge, Side};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// What identifies a revision. Blocks are by digest, so a revision costs what it changed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Revision {
    /// In sorted order, so that a merge's id does not depend on who made it.
    pub parents: Vec<String>,
    /// Empty for a merge, which is the same revision whichever machine made it.
    pub device: String,
    pub when: i64,
    pub document: Uuid,
    pub title: String,
    pub order: Vec<String>,
    pub annotations: Vec<Annotation>,
    #[serde(default)]
    pub metadata: Metadata,
}

impl Revision {
    fn id(&self) -> String {
        digest(&serde_json::to_vec(self).expect("a revision serializes"))
    }
}

/// One line of a log.
#[derive(Debug, Serialize, Deserialize)]
struct Line {
    id: String,
    revision: Revision,
    /// Blocks the revision uses that the writer had not seen in any log.
    blocks: Vec<Block>,
}

fn block_digest(block: &Block) -> String {
    digest(&serde_json::to_vec(block).expect("a block serializes"))
}

/// Everything readable from the logs in a folder.
#[derive(Debug, Default)]
pub struct Store {
    revisions: HashMap<String, Revision>,
    blocks: HashMap<String, Block>,
    /// How far each log has been read, up to the end of its last whole line, and how long it was.
    read: HashMap<PathBuf, (u64, u64)>,
    /// Whether a line came from a newer version of kame, holding something this build can't read.
    newer: bool,
    /// What has been written here and may not have reached the disk yet.
    unflushed: Unflushed,
}

/// What [`Replica::flush`] has to do.
#[derive(Debug, Default)]
struct Unflushed {
    log: bool,
    /// The log is new, so the folder has to be flushed for it to have a name.
    folder: bool,
    /// The folder is new, so its parent does, likewise.
    parent: bool,
}

/// Whether a parsed line lost something on the way in, so that it came from a newer version.
fn drops_something(raw: &[u8], line: &Line) -> bool {
    if serde_json::to_vec(line).is_ok_and(|same| same == raw) {
        return false;
    }
    let Ok(written) = serde_json::from_slice::<serde_json::Value>(raw) else {
        return false;
    };
    serde_json::to_value(line)
        .is_ok_and(|known| kame_core::model::has_unknown_fields(&written, &known))
}

impl Store {
    #[cfg(test)]
    pub(crate) fn has_unflushed(&self) -> bool {
        let unflushed = &self.unflushed;
        unflushed.log || unflushed.folder || unflushed.parent
    }

    /// Reads every log, skipping what cannot be read yet: a line still being written, or one
    /// whose bytes do not match its id.
    pub fn load(folder: &Path) -> Result<Store> {
        let mut store = Store::default();
        store.refresh(folder)?;
        Ok(store)
    }

    fn logs_in(folder: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(folder) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "log"))
            .collect();
        paths.sort();
        paths
    }

    /// Each log in a folder, with its size, sorted, from looking at the folder and reading no log.
    /// `None` if a folder or a log can't be looked at.
    pub fn log_sizes(folder: &Path) -> Option<Vec<(PathBuf, u64)>> {
        let mut sizes = Vec::new();
        for entry in std::fs::read_dir(folder).ok()? {
            let path = entry.ok()?.path();
            if path.extension().is_some_and(|extension| extension == "log") {
                let size = std::fs::metadata(&path).ok()?.len();
                sizes.push((path, size));
            }
        }
        sizes.sort();
        Some(sizes)
    }

    /// Reads what has been added to the logs since the last read, returning the ids of the
    /// revisions it found. A log that has shrunk was replaced, so it's read again from the start,
    /// which is harmless since everything is addressed by content.
    pub fn refresh(&mut self, folder: &Path) -> Result<Vec<String>> {
        let mut found = Vec::new();
        for path in Self::logs_in(folder) {
            let (mut from, _) = self.read.get(&path).copied().unwrap_or((0, 0));
            let bytes = std::fs::read(&path)?;
            if (bytes.len() as u64) < from {
                from = 0;
            }
            let fresh = &bytes[from as usize..];
            // Whatever follows the last line ending is still being written.
            let whole = fresh
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |at| at + 1);
            for raw in fresh[..whole].split(|byte| *byte == b'\n') {
                let line = match serde_json::from_slice::<Line>(raw) {
                    Ok(line) => line,
                    Err(err) => {
                        self.newer |= err.to_string().contains("unknown variant");
                        continue;
                    }
                };
                self.newer |= drops_something(raw, &line);
                if line.revision.id() != line.id {
                    continue;
                }
                for block in line.blocks {
                    self.blocks.insert(block_digest(&block), block);
                }
                if self
                    .revisions
                    .insert(line.id.clone(), line.revision)
                    .is_none()
                {
                    found.push(line.id);
                }
            }
            self.read
                .insert(path, (from + whole as u64, bytes.len() as u64));
        }
        Ok(found)
    }

    /// Whether anything read so far came from a newer version of kame. Its revisions can't be used
    /// here, and neither can anything built on them.
    pub fn holds_newer(&self) -> bool {
        self.newer
    }

    /// Whether the logs in `folder` differ from when they were last read.
    pub fn has_news(&self, folder: &Path) -> bool {
        Self::logs_in(folder).into_iter().any(|path| {
            let size = std::fs::metadata(&path).map_or(0, |found| found.len());
            self.read
                .get(&path)
                .map_or(size > 0, |(_, seen)| *seen != size)
        })
    }

    /// Revisions with everything they need here: their blocks, and parents likewise. A log can
    /// arrive before the one holding what it builds on, and then that revision waits.
    fn usable(&self) -> HashSet<String> {
        let mut waiting_on: HashMap<&String, usize> = HashMap::new();
        let mut children: HashMap<&String, Vec<&String>> = HashMap::new();
        let mut ready: Vec<&String> = Vec::new();
        for (id, revision) in &self.revisions {
            if !revision
                .order
                .iter()
                .all(|block| self.blocks.contains_key(block))
            {
                continue;
            }
            waiting_on.insert(id, revision.parents.len());
            if revision.parents.is_empty() {
                ready.push(id);
            }
            for parent in &revision.parents {
                children.entry(parent).or_default().push(id);
            }
        }
        let mut usable = HashSet::new();
        while let Some(id) = ready.pop() {
            usable.insert(id.clone());
            for child in children.get(id).into_iter().flatten() {
                if let Some(count) = waiting_on.get_mut(*child) {
                    *count -= 1;
                    if *count == 0 {
                        ready.push(child);
                    }
                }
            }
        }
        usable
    }

    /// Whether a revision can be read: it's here, and so is everything it needs.
    pub fn is_usable(&self, id: &str) -> bool {
        self.usable().contains(id)
    }

    /// Usable revisions nothing usable builds on, sorted.
    pub fn heads(&self) -> Vec<String> {
        let usable = self.usable();
        let parents: HashSet<&String> = usable
            .iter()
            .flat_map(|id| self.revisions[id].parents.iter())
            .collect();
        let mut heads: Vec<String> = usable
            .iter()
            .filter(|id| !parents.contains(id))
            .cloned()
            .collect();
        heads.sort();
        heads
    }

    pub fn document(&self, id: &str) -> Document {
        let revision = &self.revisions[id];
        Document {
            id: revision.document,
            title: revision.title.clone(),
            blocks: revision
                .order
                .iter()
                .map(|block| self.blocks[block].clone())
                .collect(),
            annotations: revision.annotations.clone(),
        }
    }

    pub fn metadata(&self, id: &str) -> Metadata {
        self.revisions[id].metadata.clone()
    }

    /// The device that wrote a revision, or the empty string for a merge.
    pub fn device(&self, id: &str) -> &str {
        &self.revisions[id].device
    }

    /// When a revision was written, by its machine's clock.
    pub fn when(&self, id: &str) -> i64 {
        self.revisions[id].when
    }

    pub(crate) fn ancestors(&self, id: &str) -> HashSet<String> {
        let mut seen = HashSet::new();
        let mut waiting = vec![id.to_string()];
        while let Some(next) = waiting.pop() {
            if let Some(revision) = self.revisions.get(&next) {
                waiting.extend(revision.parents.iter().cloned());
            }
            seen.insert(next);
        }
        seen
    }

    /// The latest revision both descend from. Where merges have crossed there can be several,
    /// and the choice only has to be the same on every machine.
    pub fn shared_ancestor(&self, one: &str, other: &str) -> Option<String> {
        let (mine, theirs) = (self.ancestors(one), self.ancestors(other));
        let common: HashSet<&String> = mine.intersection(&theirs).collect();
        // One walk up from every parent, rather than one from each, which would be quadratic.
        let mut older: HashSet<String> = HashSet::new();
        let mut waiting: Vec<&String> = common
            .iter()
            .filter_map(|id| self.revisions.get(*id))
            .flat_map(|revision| revision.parents.iter())
            .collect();
        while let Some(next) = waiting.pop() {
            if older.insert(next.clone()) {
                if let Some(revision) = self.revisions.get(next) {
                    waiting.extend(revision.parents.iter());
                }
            }
        }
        common
            .into_iter()
            .filter(|id| !older.contains(*id))
            .max()
            .cloned()
    }

    /// Who a revision's words came from, for a note to name. A merge names whoever wrote the
    /// latest of what it merged.
    fn label(&self, id: &str) -> String {
        let mut at = id.to_string();
        loop {
            let revision = &self.revisions[&at];
            if !revision.device.is_empty() {
                return crate::logs::display_device(&revision.device).to_string();
            }
            let Some(next) = revision
                .parents
                .iter()
                .max_by_key(|parent| (self.revisions[*parent].when, (*parent).clone()))
            else {
                return "another machine".to_string();
            };
            at = next.clone();
        }
    }

    pub fn revision_count(&self) -> usize {
        self.revisions.len()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.revisions.contains_key(id)
    }

    /// Whether there are revisions here and none of them are of this document.
    pub fn holds_only_others(&self, document: Uuid) -> bool {
        !self.revisions.is_empty()
            && self
                .revisions
                .values()
                .all(|revision| revision.document != document)
    }
}

/// Merges two sets of document properties field by field: a field only one side changed takes
/// that change, and one both changed takes the later. Words and accepted marks are kept from both.
pub fn merge_metadata(base: Option<&Metadata>, later: &Metadata, earlier: &Metadata) -> Metadata {
    fn field<T: Clone + PartialEq>(base: Option<&T>, later: &T, earlier: &T) -> T {
        if base == Some(later) {
            earlier.clone()
        } else {
            later.clone()
        }
    }
    let mut merged = Metadata {
        title: field(base.map(|m| &m.title), &later.title, &earlier.title),
        author: field(base.map(|m| &m.author), &later.author, &earlier.author),
        created: field(base.map(|m| &m.created), &later.created, &earlier.created),
        published: field(
            base.map(|m| &m.published),
            &later.published,
            &earlier.published,
        ),
        tags: field(base.map(|m| &m.tags), &later.tags, &earlier.tags),
        words: later.words.clone(),
        accepted: later.accepted.clone(),
        publish: field(base.map(|m| &m.publish), &later.publish, &earlier.publish),
    };
    merged.take_exceptions_from(earlier);
    merged
}

/// Whether a log has something after its last line ending.
fn ends_mid_line(log: &mut std::fs::File) -> std::io::Result<bool> {
    if log.metadata()?.len() == 0 {
        return Ok(false);
    }
    log.seek(std::io::SeekFrom::End(-1))?;
    let mut last = [0u8];
    log.read_exact(&mut last)?;
    Ok(last[0] != b'\n')
}

/// Whether a flush failed because the filesystem can't do it, rather than having tried. On macOS a
/// flush is `F_FULLFSYNC`, which what's mounted may not support, as an SMB share doesn't. The
/// numbers are for what `ErrorKind` can't tell from a disk failing: ENODEV and ENOTTY everywhere,
/// and ENOTSUP on macOS, depending on the version of Rust.
fn is_refusal(err: &std::io::Error) -> bool {
    const ENODEV: i32 = 19;
    const ENOTTY: i32 = 25;
    const MACOS_ENOTSUP: i32 = 45;
    let code = err.raw_os_error();
    matches!(
        err.kind(),
        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::Unsupported
    ) || code == Some(ENODEV)
        || code == Some(ENOTTY)
        || (cfg!(target_vendor = "apple") && code == Some(MACOS_ENOTSUP))
}

/// Takes a flush that the filesystem refused as done, which shouldn't stop a document syncing
/// there for good. Nothing has been flushed, so a power cut there can still lose the line. Anything
/// else that goes wrong is still an error.
fn unless_refused(flushed: std::io::Result<()>) -> std::io::Result<()> {
    match flushed {
        Err(err) if is_refusal(&err) => Ok(()),
        result => result,
    }
}

/// Flushes a folder, for the names in it.
fn flush_folder(folder: &Path) -> std::io::Result<()> {
    unless_refused(std::fs::File::open(folder)?.sync_all())
}

/// One machine's end of it.
pub struct Replica {
    pub folder: PathBuf,
    pub device: String,
}

#[derive(Debug)]
pub struct Synced {
    pub head: String,
    pub doc: Document,
    pub metadata: Metadata,
    /// Revisions this call wrote to this machine's log.
    pub written: usize,
    /// Notes in the document that merging added, this time.
    pub notes: usize,
}

impl Replica {
    fn log_path(&self) -> PathBuf {
        self.folder.join(format!("{}.log", self.device))
    }

    /// Writes one line at once, so a reader sees all of it or none.
    fn append(
        &self,
        store: &mut Store,
        revision: Revision,
        blocks: Vec<Block>,
    ) -> Result<(String, bool)> {
        let id = revision.id();
        let fresh: Vec<Block> = blocks
            .into_iter()
            .filter(|block| !store.blocks.contains_key(&block_digest(block)))
            .collect();
        // Already there, unless what's there is waiting on blocks in a log yet to arrive, which
        // this machine has just worked out for itself and can supply.
        if store.contains(&id) && fresh.is_empty() {
            return Ok((id, false));
        }
        let new_folder = !self.folder.exists();
        std::fs::create_dir_all(&self.folder)?;
        let new_log = !self.log_path().exists();
        let line = Line {
            id: id.clone(),
            revision: revision.clone(),
            blocks: fresh.clone(),
        };
        let mut bytes = serde_json::to_vec(&line)?;
        bytes.push(b'\n');
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(self.log_path())?;
        // A crash can leave part of a line at the end, and this one mustn't join on to it.
        if ends_mid_line(&mut log)? {
            bytes.insert(0, b'\n');
        }
        log.write_all(&bytes)?;
        store.unflushed.log = true;
        store.unflushed.folder |= new_log;
        store.unflushed.parent |= new_folder;
        // Having been read already, this machine's own line isn't news.
        let (read, seen) = store.read.entry(self.log_path()).or_insert((0, 0));
        if read == seen {
            *read += bytes.len() as u64;
            *seen = *read;
        }
        for block in fresh {
            store.blocks.insert(block_digest(&block), block);
        }
        store.revisions.insert(id.clone(), revision);
        Ok((id, true))
    }

    fn revision_of(
        doc: &Document,
        metadata: &Metadata,
        parents: Vec<String>,
        device: &str,
        when: i64,
    ) -> Revision {
        Revision {
            metadata: metadata.clone(),
            parents,
            device: device.to_string(),
            when,
            document: doc.id,
            title: doc.title.clone(),
            order: doc.blocks.iter().map(block_digest).collect(),
            annotations: doc.annotations.clone(),
        }
    }

    /// Puts what has been written to this machine's log since the last call on the disk, whether
    /// that was one line or many. Whatever fails is still to do at the next call.
    pub fn flush(&self, store: &mut Store) -> Result<()> {
        if store.unflushed.log {
            unless_refused(std::fs::File::open(self.log_path())?.sync_data())?;
            store.unflushed.log = false;
        }
        if store.unflushed.folder {
            flush_folder(&self.folder)?;
            store.unflushed.folder = false;
        }
        if store.unflushed.parent {
            if let Some(parent) = self.folder.parent() {
                flush_folder(parent)?;
            }
            store.unflushed.parent = false;
        }
        Ok(())
    }

    /// Writes a point in this document's past. It has no device in it, so every machine sharing
    /// that past writes the same revision, and their logs have ancestors in common from the start.
    pub fn write_checkpoint(
        &self,
        store: &mut Store,
        state: &Document,
        parent: Option<String>,
        at: i64,
    ) -> Result<String> {
        let parents = parent.into_iter().collect();
        let revision = Self::revision_of(state, &Metadata::default(), parents, "", at);
        Ok(self.append(store, revision, state.blocks.clone())?.0)
    }

    /// Records `local` if it has changed since `last`, the revision this machine last settled
    /// on, and merges with whatever else is in the folder until one revision remains.
    pub fn sync(&self, local: &Document, last: Option<&str>, now: i64) -> Result<Synced> {
        let mut store = Store::load(&self.folder)?;
        self.sync_in(&mut store, local, &Metadata::default(), last, now)
    }

    /// As [`Replica::sync`], with logs already in memory and the document's properties.
    pub fn sync_in(
        &self,
        store: &mut Store,
        local: &Document,
        metadata: &Metadata,
        last: Option<&str>,
        now: i64,
    ) -> Result<Synced> {
        let mut written = 0;
        // One that can't be read is no place to build from: a revision on it couldn't be read
        // either, so what's typed would be left out of the document.
        let last = last.filter(|id| store.is_usable(id));
        let changed = match last {
            Some(id) => {
                document_digest(&store.document(id)) != document_digest(local)
                    || store.revisions[id].metadata != *metadata
            }
            None => true,
        };
        if changed {
            let parents: Vec<String> = last.map(str::to_string).into_iter().collect();
            let revision = Self::revision_of(local, metadata, parents, &self.device, now);
            let (_, new) = self.append(store, revision, local.blocks.clone())?;
            written += usize::from(new);
        }
        let mut notes = 0;
        loop {
            let heads = store.heads();
            if heads.len() < 2 {
                let head = heads
                    .into_iter()
                    .next()
                    .ok_or("nothing usable in the logs yet")?;
                let doc = store.document(&head);
                let metadata = store.metadata(&head);
                return Ok(Synced {
                    head,
                    doc,
                    metadata,
                    written,
                    notes,
                });
            }
            let (one, other) = (&heads[0], &heads[1]);
            let base = store
                .shared_ancestor(one, other)
                .map(|id| store.document(&id));
            let (one_doc, other_doc) = (store.document(one), store.document(other));
            let (one_label, other_label) = (store.label(one), store.label(other));
            let (one_when, other_when) = (store.revisions[one].when, store.revisions[other].when);
            let merged = merge(
                base.as_ref(),
                Side {
                    doc: &one_doc,
                    label: &one_label,
                    when: one_when,
                },
                Side {
                    doc: &other_doc,
                    label: &other_label,
                    when: other_when,
                },
            );
            notes += merged.notes.len();
            let base_metadata = store
                .shared_ancestor(one, other)
                .map(|id| store.metadata(&id));
            // Later first, with the id settling a tie, as merging the text does.
            let (later, earlier) = if (one_when, one) > (other_when, other) {
                (one, other)
            } else {
                (other, one)
            };
            let merged_metadata = merge_metadata(
                base_metadata.as_ref(),
                &store.metadata(later),
                &store.metadata(earlier),
            );
            let revision = Self::revision_of(
                &merged.doc,
                &merged_metadata,
                vec![one.clone(), other.clone()],
                "",
                one_when.max(other_when),
            );
            let (_, new) = self.append(store, revision, merged.doc.blocks)?;
            written += usize::from(new);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revision(parents: Vec<String>, at: i64) -> Revision {
        Revision {
            parents,
            device: "test".to_string(),
            when: at,
            document: Uuid::from_u128(1),
            title: String::new(),
            order: Vec::new(),
            annotations: Vec::new(),
            metadata: Metadata::default(),
        }
    }

    fn add(store: &mut Store, revision: Revision) -> String {
        let id = revision.id();
        store.revisions.insert(id.clone(), revision);
        id
    }

    /// The definition, spelled out slowly, for the linear version to agree with: every machine
    /// has to choose the same ancestor, including machines on an older build.
    fn slow_shared_ancestor(store: &Store, one: &str, other: &str) -> Option<String> {
        let (mine, theirs) = (store.ancestors(one), store.ancestors(other));
        let common: HashSet<&String> = mine.intersection(&theirs).collect();
        let older: HashSet<String> = common
            .iter()
            .flat_map(|id| {
                store.revisions[*id]
                    .parents
                    .iter()
                    .flat_map(|parent| store.ancestors(parent))
            })
            .collect();
        common
            .into_iter()
            .filter(|id| !older.contains(*id))
            .max()
            .cloned()
    }

    #[test]
    fn the_shared_ancestor_of_crossed_merges_is_the_same_as_ever() {
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut below = move |limit: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % limit as u64) as usize
        };
        for _ in 0..40 {
            let mut store = Store::default();
            let mut ids = vec![add(&mut store, revision(Vec::new(), 0))];
            for at in 1..40 {
                // Often two parents, which is what makes merges cross.
                let mut parents = vec![ids[below(ids.len())].clone()];
                if below(2) == 0 {
                    let second = ids[below(ids.len())].clone();
                    if !parents.contains(&second) {
                        parents.push(second);
                    }
                }
                parents.sort();
                ids.push(add(&mut store, revision(parents, at)));
            }
            for _ in 0..40 {
                let (one, other) = (&ids[below(ids.len())], &ids[below(ids.len())]);
                assert_eq!(
                    store.shared_ancestor(one, other),
                    slow_shared_ancestor(&store, one, other)
                );
            }
        }
    }

    fn document_saying(words: &str) -> Document {
        Document {
            id: Uuid::from_u128(1),
            title: "t".to_string(),
            blocks: vec![Block {
                id: Uuid::from_u128(2),
                kind: kame_core::model::BlockKind::Paragraph,
                text: words.to_string(),
                spans: Vec::new(),
            }],
            annotations: Vec::new(),
        }
    }

    fn replica(folder: &Path, device: &str) -> Replica {
        Replica {
            folder: folder.to_path_buf(),
            device: device.to_string(),
        }
    }

    /// A merge whose result was written by a machine whose log hasn't arrived. What it lists is
    /// here, what it needs isn't, and merging the same two heads arrives at the same revision.
    #[test]
    fn a_merge_already_in_the_logs_without_its_blocks_does_not_stop_a_sync() {
        let folder = kame_test_dir::folder("revlog-merge-without-blocks");
        let start = replica(&folder, "one")
            .sync(&document_saying("Shared. Old."), None, 1)
            .unwrap();
        let ours = document_saying("Ours. Old.");
        let one = replica(&folder, "one")
            .sync(&ours, Some(&start.head), 2)
            .unwrap();
        let merged = replica(&folder, "two")
            .sync(&document_saying("Shared. Theirs."), Some(&start.head), 3)
            .unwrap();
        assert_eq!(merged.doc.blocks[0].text, "Ours. Theirs.");

        // The merge's line, as if its new block had gone in a third machine's log.
        let log = folder.join("two.log");
        let lines: Vec<String> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|line| {
                let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
                if value["id"] == serde_json::Value::String(merged.head.clone()) {
                    value["blocks"] = serde_json::json!([]);
                }
                value.to_string()
            })
            .collect();
        std::fs::write(&log, lines.join("\n") + "\n").unwrap();
        assert!(!Store::load(&folder).unwrap().heads().contains(&merged.head));

        let (done, finished) = std::sync::mpsc::channel();
        let (three, last) = (replica(&folder, "three"), one.head.clone());
        std::thread::spawn(move || {
            let synced = three
                .sync(&ours, Some(&last), 4)
                .map_err(|err| err.to_string());
            let _ = done.send(synced);
        });
        let synced = finished
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the sync should finish")
            .unwrap();
        assert_eq!(synced.head, merged.head, "they converge");
        assert_eq!(synced.doc.blocks[0].text, "Ours. Theirs.");
    }

    /// Every line of a log, with `change` applied to each.
    fn rewrite_log(
        log: &Path,
        mut change: impl FnMut(serde_json::Value) -> Option<serde_json::Value>,
    ) {
        let lines: Vec<String> = std::fs::read_to_string(log)
            .unwrap()
            .lines()
            .filter_map(|line| change(serde_json::from_str(line).unwrap()))
            .map(|value| value.to_string())
            .collect();
        std::fs::write(log, lines.join("\n") + "\n").unwrap();
    }

    /// The revision this machine last settled on, with its block gone, as when a log it was in has
    /// been cut short.
    #[test]
    fn a_last_revision_missing_its_block_does_not_stop_a_sync() {
        let folder = kame_test_dir::folder("revlog-last-without-block");
        let here = replica(&folder, "here");
        let last = here
            .sync(&document_saying("Written."), None, 1)
            .unwrap()
            .head;
        rewrite_log(&folder.join("here.log"), |mut value| {
            value["blocks"] = serde_json::json!([]);
            Some(value)
        });
        let typed = document_saying("Written. And more.");
        let synced = here.sync(&typed, Some(&last), 2).unwrap();
        assert_eq!(synced.doc, typed);
        assert!(Store::load(&folder).unwrap().heads().contains(&synced.head));
    }

    /// What's typed has to end up in the document, not only in a log nothing can use.
    #[test]
    fn words_typed_on_a_last_revision_missing_its_parent_are_kept() {
        let folder = kame_test_dir::folder("revlog-last-without-parent");
        let here = replica(&folder, "here");
        let parent = here.sync(&document_saying("First."), None, 1).unwrap().head;
        let last = here
            .sync(&document_saying("First. Second."), Some(&parent), 2)
            .unwrap()
            .head;
        let wanted = serde_json::Value::String(parent);
        rewrite_log(&folder.join("here.log"), |value| {
            (value["id"] != wanted).then_some(value)
        });
        replica(&folder, "there")
            .sync(&document_saying("From elsewhere."), None, 3)
            .unwrap();

        let typed = document_saying("First. Second. Typed here.");
        let synced = here.sync(&typed, Some(&last), 4).unwrap();
        let everything = format!(
            "{} {}",
            synced.doc.blocks[0].text,
            synced
                .doc
                .annotations
                .iter()
                .map(|note| note.body.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
        assert!(everything.contains("Typed here."), "{everything}");
        assert!(everything.contains("From elsewhere."), "{everything}");
    }

    #[test]
    fn a_filesystem_that_cannot_flush_is_told_from_one_that_failed() {
        use std::io::{Error, ErrorKind};
        // EINVAL and ENOTTY have these numbers on Linux and on macOS.
        assert!(is_refusal(&Error::from_raw_os_error(22)));
        assert!(is_refusal(&Error::from_raw_os_error(25)));
        assert!(is_refusal(&Error::from(ErrorKind::Unsupported)));
        if cfg!(target_vendor = "apple") {
            // ENOTSUP, then ENOSYS and EOPNOTSUPP.
            assert!(is_refusal(&Error::from_raw_os_error(45)));
            assert!(is_refusal(&Error::from_raw_os_error(78)));
            assert!(is_refusal(&Error::from_raw_os_error(102)));
        } else {
            // ENOSYS, then ENOTSUP and EOPNOTSUPP, which are one on Linux.
            assert!(is_refusal(&Error::from_raw_os_error(38)));
            assert!(is_refusal(&Error::from_raw_os_error(95)));
        }
        // EIO and ENOSPC, when the words really may not have got there.
        assert!(!is_refusal(&Error::from_raw_os_error(5)));
        assert!(!is_refusal(&Error::from_raw_os_error(28)));
        assert!(!is_refusal(&Error::from(ErrorKind::NotFound)));
    }

    #[test]
    fn a_refused_flush_counts_as_done_and_a_failed_one_does_not() {
        use std::io::Error;
        assert!(unless_refused(Ok(())).is_ok());
        assert!(unless_refused(Err(Error::from_raw_os_error(25))).is_ok());
        assert!(unless_refused(Err(Error::from_raw_os_error(5))).is_err());
    }

    /// `/dev/null` really does refuse a flush, on Linux and on macOS, each in its own words, so it
    /// stands in for a filesystem that can't.
    #[test]
    fn a_log_on_something_that_refuses_a_flush_does_not_stop_a_sync() {
        let folder = kame_test_dir::folder("revlog-refused-flush").join("logs");
        let mut store = Store::default();
        let here = replica(&folder, "here-0000");
        here.write_checkpoint(&mut store, &document_saying("Words."), None, 1)
            .unwrap();
        let log = here.log_path();
        std::fs::remove_file(&log).unwrap();
        std::os::unix::fs::symlink("/dev/null", &log).unwrap();

        here.flush(&mut store).unwrap();
        assert!(!store.has_unflushed());
    }

    #[test]
    fn what_was_written_is_flushed_once_and_a_failure_is_tried_again() {
        let folder = kame_test_dir::folder("revlog-flush").join("logs");
        let mut store = Store::default();
        let here = replica(&folder, "here-0000");
        here.write_checkpoint(&mut store, &document_saying("Words."), None, 1)
            .unwrap();
        here.write_checkpoint(&mut store, &document_saying("More."), None, 2)
            .unwrap();
        assert!(store.has_unflushed());

        let log = here.log_path();
        let held = folder.with_file_name("held.log");
        std::fs::rename(&log, &held).unwrap();
        assert!(here.flush(&mut store).is_err());
        assert!(store.has_unflushed());

        std::fs::rename(&held, &log).unwrap();
        here.flush(&mut store).unwrap();
        assert!(!store.has_unflushed());

        // With nothing written since, there's nothing to do, so the log isn't even looked at.
        std::fs::remove_file(&log).unwrap();
        here.flush(&mut store).unwrap();
    }

    /// A crash can leave half a line at the end of this machine's own log.
    #[test]
    fn a_revision_written_after_half_a_line_can_be_read() {
        let folder = kame_test_dir::folder("revlog-torn-tail");
        let here = replica(&folder, "here");
        let last = here
            .sync(&document_saying("Written."), None, 1)
            .unwrap()
            .head;
        let log = folder.join("here.log");
        let mut bytes = std::fs::read(&log).unwrap();
        bytes.extend_from_slice(b"{\"id\":\"half");
        std::fs::write(&log, bytes).unwrap();

        let synced = here
            .sync(&document_saying("Written. More."), Some(&last), 2)
            .unwrap();
        let store = Store::load(&folder).unwrap();
        assert!(store.contains(&synced.head), "the new line reads back");
        assert_eq!(store.heads(), vec![synced.head]);
    }

    /// One log line as a newer build would write it, with `extra` in the block's kind or metadata.
    fn line_from_a_newer_build(kind: &str, metadata_extra: &str) -> String {
        let block = format!(
            r#"{{"id":"00000000-0000-0000-0000-000000000010","kind":{kind},"text":"","spans":[]}}"#
        );
        let digest_of_block = digest(block.as_bytes());
        let revision = format!(
            r#"{{"parents":[],"device":"new-0000","when":1,"document":"00000000-0000-0000-0000-000000000001","title":"t","order":["{digest_of_block}"],"annotations":[],"metadata":{{"title":"","author":"","created":"","published":"","tags":[],"words":[],"accepted":[],"publish":false{metadata_extra}}}}}"#
        );
        let id = digest(revision.as_bytes());
        format!("{{\"id\":\"{id}\",\"revision\":{revision},\"blocks\":[{block}]}}\n")
    }

    fn log_holding(name: &str, text: &str) -> Store {
        let folder = kame_test_dir::folder(name);
        std::fs::write(folder.join("new-0000.log"), text).unwrap();
        Store::load(&folder).unwrap()
    }

    #[test]
    fn logs_from_a_newer_build_are_noticed() {
        let new_field = line_from_a_newer_build(r#"{"Image":{"path":"p","tilt":"Left"}}"#, "");
        assert!(log_holding("newer-field", &new_field).holds_newer());
        let in_metadata = line_from_a_newer_build(r#""Paragraph""#, r#","future":1"#);
        assert!(log_holding("newer-metadata", &in_metadata).holds_newer());
        let new_kind = line_from_a_newer_build(r#""Table""#, "");
        assert!(log_holding("newer-kind", &new_kind).holds_newer());
    }

    #[test]
    fn logs_this_build_understands_are_not_taken_for_a_newer_build() {
        let ours = line_from_a_newer_build(r#""Paragraph""#, "");
        let store = log_holding("newer-not-ours", &ours);
        assert!(!store.holds_newer());
        assert_eq!(store.heads().len(), 1);
        // Half a line, a damaged one, and an old line lacking what's been added since.
        assert!(!log_holding("newer-not-torn", "{\"id\":\"half").holds_newer());
        assert!(!log_holding("newer-not-junk", "{\"id\":1}\nnonsense\n").holds_newer());
        let older = ours.replace(
            r#","metadata":{"title":"","author":"","created":"","published":"","tags":[],"words":[],"accepted":[],"publish":false}"#,
            "",
        );
        assert_ne!(older, ours);
        assert!(!log_holding("newer-not-older", &older).holds_newer());
    }

    #[test]
    fn a_revision_waiting_on_a_missing_parent_stays_unusable() {
        let mut store = Store::default();
        let root = add(&mut store, revision(Vec::new(), 0));
        let waiting = add(&mut store, revision(vec!["not here yet".to_string()], 1));
        let after_it = add(&mut store, revision(vec![waiting.clone()], 2));
        let usable = store.usable();
        assert!(usable.contains(&root));
        assert!(!usable.contains(&waiting));
        assert!(!usable.contains(&after_it));
    }

    /// A long history has to stay quick to work with, since this runs on a pause in typing.
    #[test]
    fn a_long_history_is_quick_to_work_through() {
        let mut store = Store::default();
        let mut last = add(&mut store, revision(Vec::new(), 0));
        let split = last.clone();
        for at in 1..6000 {
            last = add(&mut store, revision(vec![last], at));
        }
        let side = add(&mut store, revision(vec![split.clone()], 6000));
        let started = std::time::Instant::now();
        assert_eq!(store.heads().len(), 2);
        assert_eq!(store.shared_ancestor(&last, &side), Some(split));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "took {:?}",
            started.elapsed()
        );
    }
}
