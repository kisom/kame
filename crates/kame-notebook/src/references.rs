//! A notebook's list of references: one `KEY.yaml` file each in `references/` under its root. The
//! key is the file name and is never written inside the file. See
//! `docs/superpowers/specs/2026-11-04-references-design.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use kame_core::references::{cited, valid_key, Reference, References};

use crate::{read_document, Notebook};

/// The folder, under the notebook's root, that holds the references.
pub const FOLDER: &str = "references";
/// What a reference's file name ends in.
pub(crate) const EXTENSION: &str = ".yaml";

/// The references a notebook holds, with what was wrong with the files that aren't among them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loaded {
    pub references: References,
    /// One line for each file that couldn't be used, naming it.
    pub problems: Vec<String>,
}

/// The folder as a path, whether or not it exists.
fn folder(notebook: &Notebook) -> PathBuf {
    notebook.root.join(FOLDER)
}

/// Whether a file of this name in the root's `references/` is a reference: a `.yaml` that isn't
/// one a sync tool or kame keeps for itself.
pub(crate) fn is_reference_name(name: &str) -> bool {
    name.ends_with(EXTENSION)
        && !name.starts_with('.')
        && !name.ends_with(".building")
        && !name.contains(".sync-conflict-")
}

/// The reference files, in order by name, as (file name, path). Symbolic links, other folders and
/// anything that isn't a reference are left out.
fn files(notebook: &Notebook) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(folder(notebook)) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            is_reference_name(&name).then(|| (name, entry.path()))
        })
        .collect();
    found.sort();
    found
}

/// The key a file name gives, if it is one.
fn key_of(name: &str) -> Option<&str> {
    name.strip_suffix(EXTENSION).filter(|key| valid_key(key))
}

/// Reads one file, or says what is wrong with it.
fn read(path: &Path) -> Result<Reference, String> {
    let yaml = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    let reference: Reference = yaml_serde::from_str(&yaml).map_err(|err| err.to_string())?;
    reference.check()?;
    Ok(reference)
}

/// Every reference the notebook has. A file that can't be used is reported and the others still
/// load. Of two keys that differ only in case, the first by name is used.
pub fn load(notebook: &Notebook) -> Loaded {
    let mut loaded = Loaded::default();
    let mut lowercase = BTreeSet::new();
    for (name, path) in files(notebook) {
        let Some(key) = key_of(&name) else {
            loaded
                .problems
                .push(format!("{FOLDER}/{name}: not a valid key"));
            continue;
        };
        if !lowercase.insert(key.to_lowercase()) {
            loaded.problems.push(format!(
                "{FOLDER}/{name}: same key as another, but for case"
            ));
            continue;
        }
        match read(&path) {
            Ok(reference) => {
                loaded.references.insert(key.to_string(), reference);
            }
            Err(err) => loaded.problems.push(format!("{FOLDER}/{name}: {err}")),
        }
    }
    loaded
}

/// Whether the file for `key` can be replaced or removed: it is absent or a good reference, and
/// no other key differs from it only in case.
fn check_writable(notebook: &Notebook, key: &str) -> Result<PathBuf, String> {
    if !valid_key(key) {
        return Err(format!("{key:?} is not a valid key"));
    }
    let wanted = format!("{key}{EXTENSION}");
    for (name, path) in files(notebook) {
        if name == wanted {
            read(&path)
                .map_err(|err| format!("{FOLDER}/{name} is damaged ({err}); fix it first"))?;
        } else if name.to_lowercase() == wanted.to_lowercase() {
            return Err(format!("{FOLDER}/{name} has the same key, but for case"));
        }
    }
    Ok(folder(notebook).join(wanted))
}

/// Writes `reference` as `key`, through a temporary file renamed into place so that an interrupted
/// save leaves the old one. A damaged file for `key` is refused and left as it was.
pub fn save(notebook: &Notebook, key: &str, reference: &Reference) -> Result<(), String> {
    reference.check()?;
    let path = check_writable(notebook, key)?;
    let yaml = yaml_serde::to_string(reference).map_err(|err| err.to_string())?;
    let folder = folder(notebook);
    std::fs::create_dir_all(&folder)
        .map_err(|err| format!("could not make {}: {err}", folder.display()))?;
    let temporary = folder.join(format!(".{key}{EXTENSION}.saving"));
    std::fs::write(&temporary, yaml)
        .map_err(|err| format!("could not write {}: {err}", temporary.display()))?;
    std::fs::rename(&temporary, &path).map_err(|err| {
        let _ = std::fs::remove_file(&temporary);
        format!("could not write {}: {err}", path.display())
    })
}

/// Removes the reference `key`. One that is already gone is fine; a damaged file is refused.
pub fn remove(notebook: &Notebook, key: &str) -> Result<(), String> {
    let path = check_writable(notebook, key)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("could not remove {}: {err}", path.display())),
    }
}

/// The documents that cite each reference, once each, counting footnotes and margin notes but
/// not private notes. It reads every document, so it is for a background thread.
pub fn citing(notebook: &Notebook) -> BTreeMap<String, Vec<PathBuf>> {
    let loaded = load(notebook).references;
    let mut found: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    if loaded.is_empty() {
        return found;
    }
    for entry in notebook.documents() {
        let Some((doc, _)) = read_document(&entry.path) else {
            continue;
        };
        for (key, _) in cited(&doc, &loaded, true) {
            found
                .entry(key.to_string())
                .or_default()
                .push(entry.path.clone());
        }
    }
    found
}

/// The name, time and length of each reference file, so that a change to the list shows without
/// reading it.
pub fn stamp(notebook: &Notebook) -> Vec<(String, SystemTime, u64)> {
    files(notebook)
        .into_iter()
        .filter_map(|(name, path)| {
            let metadata = std::fs::metadata(path).ok()?;
            Some((name, metadata.modified().ok()?, metadata.len()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kame_core::model::{Anchor, Annotation, AnnotationKind, Document};
    use kame_core::references::Kind;
    use kame_core::storage::Db;

    fn notebook(name: &str) -> Notebook {
        let root = kame_test_dir::folder(name);
        std::fs::write(root.join(crate::SETTINGS), "").unwrap();
        Notebook::open(&root).unwrap()
    }

    fn put(notebook: &Notebook, name: &str, text: &str) -> PathBuf {
        let folder = folder(notebook);
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn article() -> Reference {
        Reference {
            kind: Kind::Article,
            authors: vec!["D. E. Knuth".to_string()],
            title: "Breaking paragraphs into lines".to_string(),
            published_in: "Softw.: Pract. Exper.".to_string(),
            year: "1981".to_string(),
            ..Reference::default()
        }
    }

    #[test]
    fn a_reference_comes_back_as_it_was_saved() {
        let notebook = notebook("ref-roundtrip");
        let freeform = Reference {
            kind: Kind::Freeform,
            text: "A letter, 1 May 1984.\nSecond line: with a colon.\n".to_string(),
            ..Reference::default()
        };
        save(&notebook, "knuth1981", &article()).unwrap();
        save(&notebook, "letter", &freeform).unwrap();
        let loaded = load(&notebook);
        assert_eq!(loaded.problems, Vec::<String>::new());
        assert_eq!(loaded.references.get("knuth1981"), Some(&article()));
        assert_eq!(loaded.references.get("letter"), Some(&freeform));
    }

    #[test]
    fn the_key_is_the_file_name_and_never_in_the_file() {
        let notebook = notebook("ref-key");
        save(&notebook, "knuth1981", &article()).unwrap();
        let yaml = std::fs::read_to_string(folder(&notebook).join("knuth1981.yaml")).unwrap();
        assert!(!yaml.contains("key"), "{yaml}");
        assert!(!yaml.contains("knuth1981"), "{yaml}");
        assert!(yaml.contains("kind: article"), "{yaml}");
        for empty in ["editors", "volume", "doi", "url", "text"] {
            assert!(!yaml.contains(empty), "{empty} in {yaml}");
        }
    }

    #[test]
    fn a_bare_number_reads_as_text() {
        let notebook = notebook("ref-number");
        put(
            &notebook,
            "knuth1981.yaml",
            "kind: article\ntitle: x\nvolume: 11\nyear: 1981\n",
        );
        let loaded = load(&notebook);
        assert_eq!(loaded.problems, Vec::<String>::new());
        let reference = loaded.references.get("knuth1981").unwrap();
        assert_eq!(reference.year, "1981");
        assert_eq!(reference.volume, "11");
    }

    #[test]
    fn a_damaged_file_is_reported_and_the_others_load() {
        let notebook = notebook("ref-damaged");
        save(&notebook, "good", &article()).unwrap();
        put(&notebook, "kind.yaml", "kind: nonsense\ntitle: x\n");
        put(
            &notebook,
            "unknown.yaml",
            "kind: article\ntitle: x\nbogus: 1\n",
        );
        put(
            &notebook,
            "volume.yaml",
            "kind: book\ntitle: x\nvolume: 3\n",
        );
        let loaded = load(&notebook);
        assert_eq!(loaded.references.keys().collect::<Vec<_>>(), ["good"]);
        assert_eq!(loaded.problems.len(), 3, "{:?}", loaded.problems);
        for name in ["kind.yaml", "unknown.yaml", "volume.yaml"] {
            let problem = loaded.problems.iter().find(|line| line.contains(name));
            assert!(problem.is_some(), "{name} in {:?}", loaded.problems);
        }
        let volume = loaded
            .problems
            .iter()
            .find(|line| line.contains("volume.yaml"));
        assert!(volume.unwrap().contains("volume"), "{volume:?}");
    }

    #[test]
    fn a_damaged_file_is_never_overwritten() {
        let notebook = notebook("ref-keep-damaged");
        let path = put(&notebook, "broken.yaml", "kind: nonsense\n");
        assert!(save(&notebook, "broken", &article()).is_err());
        assert!(remove(&notebook, "broken").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"kind: nonsense\n");
    }

    #[test]
    fn keys_differing_only_in_case_cant_be_saved() {
        let notebook = notebook("ref-case-save");
        save(&notebook, "Knuth", &article()).unwrap();
        assert!(save(&notebook, "knuth", &article()).is_err());
        let names: Vec<String> = stamp(&notebook)
            .into_iter()
            .map(|(name, ..)| name)
            .collect();
        assert_eq!(names, ["Knuth.yaml"]);
        // The same key again is an update.
        save(&notebook, "Knuth", &article()).unwrap();
    }

    #[test]
    fn two_files_differing_only_in_case_use_the_first_and_report_the_other() {
        let notebook = notebook("ref-case-load");
        let yaml = "kind: freeform\ntext: x\n";
        put(&notebook, "Knuth.yaml", yaml);
        put(&notebook, "knuth.yaml", yaml);
        if files(&notebook).len() < 2 {
            return; // A filesystem that ignores case can't hold both.
        }
        let loaded = load(&notebook);
        assert_eq!(loaded.references.keys().collect::<Vec<_>>(), ["Knuth"]);
        assert_eq!(loaded.problems.len(), 1);
        assert!(loaded.problems[0].contains("knuth.yaml"));
    }

    #[test]
    fn what_isnt_a_reference_is_left_out() {
        let notebook = notebook("ref-left-out");
        let yaml = "kind: freeform\ntext: x\n";
        put(&notebook, "kept.yaml", yaml);
        put(&notebook, ".hidden.yaml", yaml);
        put(&notebook, "a.sync-conflict-20260101-000000-ABC.yaml", yaml);
        put(&notebook, "x.yaml.building", yaml);
        put(&notebook, "notes.txt", yaml);
        std::fs::create_dir(folder(&notebook).join("dir.yaml")).unwrap();
        let outside = kame_test_dir::folder("ref-outside").join("linked.yaml");
        std::fs::write(&outside, yaml).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, folder(&notebook).join("linked.yaml")).unwrap();
        put(&notebook, "1.yaml", yaml);
        let loaded = load(&notebook);
        assert_eq!(loaded.references.keys().collect::<Vec<_>>(), ["kept"]);
        assert_eq!(loaded.problems.len(), 1, "{:?}", loaded.problems);
        assert!(loaded.problems[0].contains("1.yaml"));
        let names: Vec<String> = stamp(&notebook)
            .into_iter()
            .map(|(name, ..)| name)
            .collect();
        assert_eq!(names, ["1.yaml", "kept.yaml"]);
    }

    #[test]
    fn an_interrupted_save_leaves_the_old_file() {
        let notebook = notebook("ref-interrupted");
        save(&notebook, "knuth1981", &article()).unwrap();
        // What a crash between writing and renaming leaves: a hidden temporary file.
        put(
            &notebook,
            ".knuth1981.yaml.saving",
            "kind: article\ntitle: half",
        );
        let loaded = load(&notebook);
        assert_eq!(loaded.problems, Vec::<String>::new());
        assert_eq!(loaded.references.get("knuth1981"), Some(&article()));
        // A finished save leaves no temporary file behind.
        save(&notebook, "other", &article()).unwrap();
        let temporary = folder(&notebook).join(".other.yaml.saving");
        assert!(!temporary.exists());
    }

    #[test]
    fn a_notebook_with_no_folder_loads_nothing() {
        let notebook = notebook("ref-none");
        assert_eq!(load(&notebook), Loaded::default());
        assert!(stamp(&notebook).is_empty());
        assert!(citing(&notebook).is_empty());
        remove(&notebook, "absent").unwrap();
    }

    #[test]
    fn a_removed_reference_is_gone() {
        let notebook = notebook("ref-remove");
        save(&notebook, "knuth1981", &article()).unwrap();
        remove(&notebook, "knuth1981").unwrap();
        assert!(load(&notebook).references.is_empty());
    }

    #[test]
    fn a_save_changes_the_stamp() {
        let notebook = notebook("ref-stamp");
        let before = stamp(&notebook);
        save(&notebook, "knuth1981", &article()).unwrap();
        let after = stamp(&notebook);
        assert_ne!(before, after);
        assert_eq!(after[0].0, "knuth1981.yaml");
    }

    fn write_document(path: &Path, text: &str, notes: &[(AnnotationKind, &str)]) {
        let mut doc = Document::new("T");
        doc.blocks[0].text = text.to_string();
        let block = doc.blocks[0].id;
        for (kind, body) in notes {
            doc.annotations.push(Annotation {
                id: uuid::Uuid::new_v4(),
                kind: *kind,
                anchor: Anchor::block(block),
                body: body.to_string(),
            });
        }
        let mut db = Db::open(path).unwrap();
        db.save_document(&doc).unwrap();
    }

    #[test]
    fn citing_finds_each_document_once_counting_footnotes_and_marginals_but_not_notes() {
        let notebook = notebook("ref-citing");
        for key in ["alpha", "beta", "gamma", "delta"] {
            save(&notebook, key, &article()).unwrap();
        }
        let one = notebook.root.join("one.kame");
        write_document(
            &one,
            "see [alpha] and again [alpha] and [unknown]",
            &[
                (AnnotationKind::Footnote, "[beta]"),
                (AnnotationKind::Marginal, "[gamma]"),
                (AnnotationKind::Note, "[delta]"),
            ],
        );
        let two = notebook.root.join("two.md");
        std::fs::write(&two, "Also [alpha].\n").unwrap();
        let found = citing(&notebook);
        assert_eq!(
            found.keys().map(String::as_str).collect::<Vec<_>>(),
            ["alpha", "beta", "gamma"]
        );
        assert_eq!(found["alpha"], [one.clone(), two]);
        assert_eq!(found["beta"], std::slice::from_ref(&one));
        assert_eq!(found["gamma"], std::slice::from_ref(&one));
    }
}
