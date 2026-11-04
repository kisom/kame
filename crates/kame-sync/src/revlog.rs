//! Revisions in logs that each have one writer, carried by anything that copies files.
//!
//! A shared database cannot survive file sync, since two machines write one file. Here every
//! machine appends only to its own log, so a sync tool never sees a conflict, and a log that
//! arrives late or partly written costs nothing. Revisions form a graph, as in git, and are
//! addressed by content. A merge's id depends only on what it merges and its result, so two
//! machines that merge separately write the same revision, and they have converged.

use std::collections::{HashMap, HashSet};
use std::io::Write;
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
}

impl Store {
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
                let Ok(line) = serde_json::from_slice::<Line>(raw) else {
                    continue;
                };
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
    };
    merged.take_exceptions_from(earlier);
    merged
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
        if store.contains(&id) {
            return Ok((id, false));
        }
        std::fs::create_dir_all(&self.folder)?;
        let fresh: Vec<Block> = blocks
            .into_iter()
            .filter(|block| !store.blocks.contains_key(&block_digest(block)))
            .collect();
        let line = Line {
            id: id.clone(),
            revision: revision.clone(),
            blocks: fresh.clone(),
        };
        let mut bytes = serde_json::to_vec(&line)?;
        bytes.push(b'\n');
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())?;
        log.write_all(&bytes)?;
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
        let changed = match last {
            Some(id) if store.contains(id) => {
                document_digest(&store.document(id)) != document_digest(local)
                    || store.revisions[id].metadata != *metadata
            }
            _ => true,
        };
        if changed {
            let parents: Vec<String> = last
                .filter(|id| store.contains(id))
                .map(str::to_string)
                .into_iter()
                .collect();
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
