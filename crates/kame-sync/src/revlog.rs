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

use kame_core::model::{Annotation, Block, Document};
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
}

impl Store {
    /// Reads every log, skipping what cannot be read yet: a line still being written, or one
    /// whose bytes do not match its id.
    pub fn load(folder: &Path) -> Result<Store> {
        let mut store = Store::default();
        if !folder.exists() {
            return Ok(store);
        }
        let mut paths: Vec<PathBuf> = std::fs::read_dir(folder)?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "log"))
            .collect();
        paths.sort();
        for path in paths {
            let bytes = std::fs::read(&path)?;
            for raw in bytes.split(|byte| *byte == b'\n') {
                let Ok(line) = serde_json::from_slice::<Line>(raw) else {
                    continue;
                };
                if line.revision.id() != line.id {
                    continue;
                }
                for block in line.blocks {
                    store.blocks.insert(block_digest(&block), block);
                }
                store.revisions.insert(line.id, line.revision);
            }
        }
        Ok(store)
    }

    /// Revisions with everything they need here: their blocks, and parents likewise. A log can
    /// arrive before the one holding what it builds on, and then that revision waits.
    fn usable(&self) -> HashSet<String> {
        let mut usable: HashSet<String> = HashSet::new();
        loop {
            let before = usable.len();
            for (id, revision) in &self.revisions {
                if !usable.contains(id)
                    && revision
                        .parents
                        .iter()
                        .all(|parent| usable.contains(parent))
                    && revision
                        .order
                        .iter()
                        .all(|block| self.blocks.contains_key(block))
                {
                    usable.insert(id.clone());
                }
            }
            if usable.len() == before {
                return usable;
            }
        }
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

    fn ancestors(&self, id: &str) -> HashSet<String> {
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
        let older: HashSet<String> = common
            .iter()
            .flat_map(|id| {
                self.revisions[*id]
                    .parents
                    .iter()
                    .flat_map(|parent| self.ancestors(parent))
            })
            .collect();
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
                return revision.device.clone();
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

/// One machine's end of it.
pub struct Replica {
    pub folder: PathBuf,
    pub device: String,
}

#[derive(Debug)]
pub struct Synced {
    pub head: String,
    pub doc: Document,
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
        for block in fresh {
            store.blocks.insert(block_digest(&block), block);
        }
        store.revisions.insert(id.clone(), revision);
        Ok((id, true))
    }

    fn revision_of(doc: &Document, parents: Vec<String>, device: &str, when: i64) -> Revision {
        Revision {
            parents,
            device: device.to_string(),
            when,
            document: doc.id,
            title: doc.title.clone(),
            order: doc.blocks.iter().map(block_digest).collect(),
            annotations: doc.annotations.clone(),
        }
    }

    /// Records `local` if it has changed since `last`, the revision this machine last settled
    /// on, and merges with whatever else is in the folder until one revision remains.
    pub fn sync(&self, local: &Document, last: Option<&str>, now: i64) -> Result<Synced> {
        let mut store = Store::load(&self.folder)?;
        let mut written = 0;
        let changed = match last {
            Some(id) if store.contains(id) => {
                document_digest(&store.document(id)) != document_digest(local)
            }
            _ => true,
        };
        if changed {
            let parents: Vec<String> = last
                .filter(|id| store.contains(id))
                .map(str::to_string)
                .into_iter()
                .collect();
            let revision = Self::revision_of(local, parents, &self.device, now);
            let (_, new) = self.append(&mut store, revision, local.blocks.clone())?;
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
                return Ok(Synced {
                    head,
                    doc,
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
            let revision = Self::revision_of(
                &merged.doc,
                vec![one.clone(), other.clone()],
                "",
                one_when.max(other_when),
            );
            let (_, new) = self.append(&mut store, revision, merged.doc.blocks)?;
            written += usize::from(new);
        }
    }
}
