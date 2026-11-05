//! A notebook's list of references: one `KEY.yaml` file each in `references/` under its root. The
//! key is the file name and is never written inside the file. See
//! `docs/superpowers/specs/2026-11-04-references-design.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use kame_core::references::{cited, valid_key, Reference, References};
use kame_core::session::{is_markdown_path, Session};

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
/// hidden or a sync tool's conflict copy.
pub(crate) fn is_reference_name(name: &str) -> bool {
    name.ends_with(EXTENSION) && !name.starts_with('.') && !name.contains(".sync-conflict-")
}

/// Whether the folder is there to use. `Ok(false)` is a folder not made yet, and `Err` says why a
/// path that is there can't be used: a symbolic link, which a synced notebook could point
/// anywhere, or something that isn't a folder.
fn folder_ready(notebook: &Notebook) -> Result<bool, String> {
    let folder = folder(notebook);
    match std::fs::symlink_metadata(&folder) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(metadata) if metadata.is_symlink() => Err(format!(
            "{FOLDER} is a symbolic link, which is not followed"
        )),
        Ok(_) => Err(format!("{FOLDER} is not a folder")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("could not read {FOLDER}: {err}")),
    }
}

/// The reference files, in order by name, as (file name, path). Symbolic links, other folders and
/// anything that isn't a reference are left out.
fn files(notebook: &Notebook) -> Vec<(String, PathBuf)> {
    if !folder_ready(notebook).unwrap_or(false) {
        return Vec::new();
    }
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
    if let Err(problem) = folder_ready(notebook) {
        loaded.problems.push(problem);
        return loaded;
    }
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

/// Whether the file for `key` can be replaced or removed: the folder is usable, and the file is
/// absent or a good reference and not a symbolic link, and no other key differs from it only in
/// case.
fn check_writable(notebook: &Notebook, key: &str) -> Result<PathBuf, String> {
    if !valid_key(key) {
        return Err(format!("{key:?} is not a valid key"));
    }
    folder_ready(notebook)?;
    let wanted = format!("{key}{EXTENSION}");
    let path = folder(notebook).join(&wanted);
    if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_symlink()) {
        return Err(format!("{FOLDER}/{wanted} is a symbolic link"));
    }
    for (name, path) in files(notebook) {
        if name == wanted {
            read(&path)
                .map_err(|err| format!("{FOLDER}/{name} is damaged ({err}); fix it first"))?;
        } else if name.to_lowercase() == wanted.to_lowercase() {
            return Err(format!("{FOLDER}/{name} has the same key, but for case"));
        }
    }
    Ok(path)
}

/// How old a leftover temporary file has to be before a save removes it, so that no other kame
/// can still be writing it.
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Removes the temporary files an interrupted save left in `folder`: hidden files with
/// `.saving-` in their names, not touched for a day. A symbolic link or anything that isn't a
/// plain file is left alone, and a file that can't be removed is left for next time.
fn clear_stale_temporaries(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') || !name.contains(&format!("{EXTENSION}.saving-")) {
            continue;
        }
        let stale = entry
            .path()
            .symlink_metadata()
            .ok()
            .filter(|metadata| metadata.file_type().is_file())
            .and_then(|metadata| metadata.modified().ok())
            .is_some_and(|modified| {
                now.duration_since(modified)
                    .is_ok_and(|age| age >= STALE_AFTER)
            });
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Writes `reference` as `key`, through a temporary file renamed into place so that an interrupted
/// save leaves the old one, and a save clears the temporary files an earlier crash left. A damaged file for `key` is refused and left as it was.
pub fn save(notebook: &Notebook, key: &str, reference: &Reference) -> Result<(), String> {
    reference.check()?;
    let path = check_writable(notebook, key)?;
    let yaml = yaml_serde::to_string(reference).map_err(|err| err.to_string())?;
    let folder = folder(notebook);
    // `check_writable` found the folder absent or a real one, so this never goes through a link.
    std::fs::create_dir_all(&folder)
        .map_err(|err| format!("could not make {}: {err}", folder.display()))?;
    clear_stale_temporaries(&folder);
    // A name nobody can have planted, made new and never followed, and hidden so that listing
    // skips it.
    let temporary = folder.join(format!(".{key}{EXTENSION}.saving-{}", uuid::Uuid::new_v4()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(yaml.as_bytes())?;
            // So that a power loss can't leave the new name on an empty file.
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&temporary, &path));
    written.map_err(|err| {
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

/// What a rename did to the documents that cite the old key.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Renamed {
    /// The documents now citing the new key.
    pub rewritten: Vec<PathBuf>,
    /// The documents that may still cite the old one, each with why.
    pub failed: Vec<(PathBuf, String)>,
    /// Whether the old key's file is still there, so that `[old]` still resolves: because a
    /// document failed, because the file couldn't be removed, or because it's left for whoever
    /// has a document open.
    pub old_kept: bool,
}

/// Whether two paths are one file, comparing them as the system resolves them where it can.
fn same_file(one: &Path, other: &Path) -> bool {
    match (one.canonicalize(), other.canonicalize()) {
        (Ok(one), Ok(other)) => one == other,
        _ => one == other,
    }
}

/// Rewrites the citations in one document, which must be a `.kame` file or have only logs.
fn rewrite(path: &Path, old: &str, new: &str) -> Result<(), String> {
    if is_markdown_path(path) {
        return Err("a Markdown document can't be written by kame".to_string());
    }
    // Opening a path with no file would make a blank document over the text in its logs.
    let built = kame_sync::logs::build_from_logs(path).map_err(|err| err.to_string())?;
    if !built && !path.exists() {
        return Err("its logs don't hold a whole document".to_string());
    }
    let mut session = Session::open(path).map_err(|err| err.to_string())?;
    if session.newer {
        return Err("it has changes from a newer version of kame".to_string());
    }
    if session.rename_citation(old, new) == 0 || session.save_failed {
        return Err("its citations could not be rewritten".to_string());
    }
    Ok(())
}

/// Renames the reference `old` to `new`, and the citations of it in every document that has
/// them except `open`, which the app rewrites through its own session. The new file is written
/// first and the old one removed last, so an interruption leaves both and loses nothing. With an
/// `open` document the old file is left, as that document still holds `[old]`, and whoever has it
/// open removes the file with [`remove`] once it has followed. A key that differs from `old` or
/// any other only in case is refused, as is a damaged file for either. A `new` that already holds
/// exactly `old`'s reference is accepted, so that a rename can be run again after fixing what
/// stopped it. A document that can't be rewritten, or read to see whether it cites `old`, is named
/// in the result and the rest still are; the old file is then kept.
pub fn rename(
    notebook: &Notebook,
    old: &str,
    new: &str,
    open: Option<&Path>,
) -> Result<Renamed, String> {
    if !valid_key(new) {
        return Err(format!("{new:?} is not a valid key"));
    }
    let loaded = load(notebook).references;
    let reference = loaded
        .get(old)
        .cloned()
        .ok_or_else(|| format!("there is no reference {old:?}"))?;
    if new.eq_ignore_ascii_case(old) {
        return Err(format!("{new:?} is already a reference"));
    }
    let taken = format!("{new}{EXTENSION}").to_lowercase();
    let in_use = files(notebook)
        .iter()
        .any(|(name, _)| name.to_lowercase() == taken);
    let same = loaded.get(new) == Some(&reference);
    if in_use && !same {
        return Err(format!("{new:?} is already a reference"));
    }
    if !same {
        save(notebook, new, &reference)?;
    }
    let mut renamed = Renamed::default();
    for entry in notebook.documents() {
        let path = entry.path;
        if open.is_some_and(|open| same_file(open, &path)) {
            continue;
        }
        let Some((doc, _)) = read_document(&path) else {
            renamed
                .failed
                .push((path, "couldn't be read, so it may cite this".to_string()));
            continue;
        };
        if !cited(&doc, &loaded, true)
            .iter()
            .any(|(key, _)| *key == old)
        {
            continue;
        }
        match rewrite(&path, old, new) {
            Ok(()) => renamed.rewritten.push(path),
            Err(why) => renamed.failed.push((path, why)),
        }
    }
    renamed.old_kept =
        open.is_some() || !renamed.failed.is_empty() || remove(notebook, old).is_err();
    Ok(renamed)
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
    fn an_unquoted_number_keeps_the_digits_typed() {
        let notebook = notebook("ref-digits");
        put(
            &notebook,
            "knuth1981.yaml",
            "kind: article\ntitle: x\nvolume: 2.10\nnumber: 007\npages: 1e3\nyear: 1981\n",
        );
        let loaded = load(&notebook);
        assert_eq!(loaded.problems, Vec::<String>::new());
        let reference = loaded.references.get("knuth1981").unwrap();
        assert_eq!(reference.volume, "2.10");
        assert_eq!(reference.number, "007");
        assert_eq!(reference.pages, "1e3");
        assert_eq!(reference.year, "1981");
    }

    #[test]
    fn renaming_a_key_that_is_not_a_reference_says_so_even_for_a_change_of_case() {
        let notebook = notebook("ref-rename-missing-case");
        save(&notebook, "old", &article()).unwrap();
        let err = rename(&notebook, "missing", "MISSING", None).unwrap_err();
        assert_eq!(err, "there is no reference \"missing\"");
        let err = rename(&notebook, "old", "OLD", None).unwrap_err();
        assert_eq!(err, "\"OLD\" is already a reference");
    }

    fn aged(path: &Path, age: std::time::Duration) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - age).unwrap();
    }

    #[test]
    fn a_save_removes_old_leftover_temporary_files_and_no_others() {
        let notebook = notebook("ref-stale");
        let day = std::time::Duration::from_secs(24 * 60 * 60);
        let old = put(&notebook, ".a.yaml.saving-1", "x");
        aged(&old, day * 2);
        let young = put(&notebook, ".b.yaml.saving-2", "x");
        aged(&young, day / 2);
        let ordinary = put(&notebook, ".c.yaml", "x");
        aged(&ordinary, day * 2);
        let visible = put(&notebook, "d.yaml.saving-3", "x");
        aged(&visible, day * 2);
        let target = notebook.root.join("outside.txt");
        std::fs::write(&target, "x").unwrap();
        aged(&target, day * 2);
        #[cfg(unix)]
        let link = {
            let link = folder(&notebook).join(".e.yaml.saving-4");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            link
        };
        let folder_path = folder(&notebook);
        std::fs::create_dir(folder_path.join(".f.yaml.saving-5")).unwrap();

        save(&notebook, "knuth1981", &article()).unwrap();

        assert!(!old.exists(), "an old temporary file stays");
        assert!(young.exists(), "a young one goes");
        assert!(ordinary.exists() && visible.exists() && target.exists());
        #[cfg(unix)]
        assert!(link.symlink_metadata().is_ok(), "a link goes");
        assert!(folder_path.join(".f.yaml.saving-5").is_dir());
        assert!(folder_path.join("knuth1981.yaml").is_file());
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

    #[cfg(unix)]
    #[test]
    fn a_link_planted_where_a_save_used_to_write_is_not_followed() {
        let notebook = notebook("ref-planted-temporary");
        let target = kame_test_dir::folder("ref-planted-target").join("victim");
        std::fs::write(&target, "precious").unwrap();
        let folder = folder(&notebook);
        std::fs::create_dir_all(&folder).unwrap();
        std::os::unix::fs::symlink(&target, folder.join(".knuth1981.yaml.saving")).unwrap();
        save(&notebook, "knuth1981", &article()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"precious");
        assert_eq!(
            load(&notebook).references.get("knuth1981"),
            Some(&article())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_folder_is_not_followed() {
        let notebook = notebook("ref-linked-folder");
        let outside = kame_test_dir::folder("ref-linked-outside");
        let yaml = "kind: freeform\ntext: x\n";
        std::fs::write(outside.join("there.yaml"), yaml).unwrap();
        std::os::unix::fs::symlink(&outside, folder(&notebook)).unwrap();
        let loaded = load(&notebook);
        assert!(loaded.references.is_empty());
        assert_eq!(loaded.problems.len(), 1, "{:?}", loaded.problems);
        assert!(loaded.problems[0].contains(FOLDER));
        assert!(save(&notebook, "knuth1981", &article()).is_err());
        assert!(remove(&notebook, "there").is_err());
        assert!(stamp(&notebook).is_empty());
        assert!(citing(&notebook).is_empty());
        let mut left: Vec<String> = std::fs::read_dir(&outside)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["there.yaml"]);
        assert_eq!(
            std::fs::read_to_string(outside.join("there.yaml")).unwrap(),
            yaml
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_at_a_keys_file_is_neither_replaced_nor_removed() {
        let notebook = notebook("ref-linked-file");
        let target = kame_test_dir::folder("ref-linked-file-target").join("victim.yaml");
        std::fs::write(&target, "kind: freeform\ntext: x\n").unwrap();
        let folder = folder(&notebook);
        std::fs::create_dir_all(&folder).unwrap();
        let link = folder.join("knuth1981.yaml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(save(&notebook, "knuth1981", &article()).is_err());
        assert!(remove(&notebook, "knuth1981").is_err());
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"kind: freeform\ntext: x\n"
        );
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
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
        let hidden = std::fs::read_dir(folder(&notebook))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with('.'))
            .count();
        assert_eq!(hidden, 1, "only the planted one");
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

    fn text_of(path: &Path) -> String {
        read_document(path).unwrap().0.blocks[0].text.clone()
    }

    fn body_of(path: &Path) -> String {
        read_document(path).unwrap().0.annotations[0].body.clone()
    }

    #[test]
    fn renaming_rewrites_each_document_and_moves_the_file() {
        let notebook = notebook("ref-rename");
        save(&notebook, "old", &article()).unwrap();
        let one = notebook.root.join("one.kame");
        let two = notebook.root.join("two.kame");
        let bystander = notebook.root.join("bystander.kame");
        write_document(
            &one,
            "see [old]",
            &[(AnnotationKind::Footnote, "also [old]")],
        );
        write_document(&two, "and [old] again", &[]);
        write_document(&bystander, "no [other] here", &[]);
        let renamed = rename(&notebook, "old", "fresh", None).unwrap();
        assert_eq!(renamed.failed, []);
        assert_eq!(renamed.rewritten, [one.clone(), two.clone()]);
        assert_eq!(text_of(&one), "see [fresh]");
        assert_eq!(body_of(&one), "also [fresh]");
        assert_eq!(text_of(&two), "and [fresh] again");
        assert_eq!(text_of(&bystander), "no [other] here");
        let loaded = load(&notebook);
        assert_eq!(loaded.references.keys().collect::<Vec<_>>(), ["fresh"]);
        assert_eq!(loaded.references.get("fresh"), Some(&article()));
        assert!(!folder(&notebook).join("old.yaml").exists());
    }

    #[test]
    fn renaming_skips_the_open_document_and_says_nothing_of_it() {
        let notebook = notebook("ref-rename-open");
        save(&notebook, "old", &article()).unwrap();
        let open = notebook.root.join("open.kame");
        let other = notebook.root.join("other.kame");
        write_document(&open, "[old]", &[]);
        write_document(&other, "[old]", &[]);
        let renamed = rename(&notebook, "old", "fresh", Some(&open)).unwrap();
        assert_eq!(renamed.rewritten, std::slice::from_ref(&other));
        assert_eq!(renamed.failed, []);
        assert_eq!(text_of(&open), "[old]");
        assert_eq!(text_of(&other), "[fresh]");
        assert!(folder(&notebook).join("fresh.yaml").exists());
        // `[old]` is still in the open document, so its file waits for that to follow.
        assert!(folder(&notebook).join("old.yaml").exists());
        assert!(renamed.old_kept);
    }

    #[test]
    fn renaming_with_nothing_open_removes_the_old_file_itself() {
        let notebook = notebook("ref-rename-none-open");
        save(&notebook, "old", &article()).unwrap();
        let renamed = rename(&notebook, "old", "fresh", None).unwrap();
        assert!(!renamed.old_kept);
        assert!(!folder(&notebook).join("old.yaml").exists());
    }

    #[test]
    fn renaming_names_a_markdown_document_it_cant_write_and_keeps_going() {
        let notebook = notebook("ref-rename-markdown");
        save(&notebook, "old", &article()).unwrap();
        let note = notebook.root.join("a.md");
        let kame = notebook.root.join("b.kame");
        std::fs::write(&note, "Cites [old].\n").unwrap();
        write_document(&kame, "[old]", &[]);
        let renamed = rename(&notebook, "old", "fresh", None).unwrap();
        assert_eq!(renamed.rewritten, std::slice::from_ref(&kame));
        assert_eq!(renamed.failed.len(), 1, "{:?}", renamed.failed);
        assert_eq!(renamed.failed[0].0, note);
        assert!(
            renamed.failed[0].1.contains("Markdown"),
            "{:?}",
            renamed.failed
        );
        assert_eq!(std::fs::read_to_string(&note).unwrap(), "Cites [old].\n");
        assert_eq!(text_of(&kame), "[fresh]");
    }

    #[test]
    fn renaming_names_a_newer_document_it_cant_write() {
        let notebook = notebook("ref-rename-broken");
        save(&notebook, "old", &article()).unwrap();
        let good = notebook.root.join("a.kame");
        write_document(&good, "[old]", &[]);
        // A document that cites it, as far as reading goes, but is read-only here.
        let newer = notebook.root.join("b.kame");
        write_document(&newer, "[old]", &[]);
        let connection = rusqlite::Connection::open(&newer).unwrap();
        let head: String = connection
            .query_row("SELECT json FROM documents WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let future = head.replacen("\"spans\":[]", "\"spans\":[],\"future\":1", 1);
        assert_ne!(future, head);
        connection
            .execute("UPDATE documents SET json = ?1 WHERE id = 1", [&future])
            .unwrap();
        drop(connection);
        let renamed = rename(&notebook, "old", "fresh", None).unwrap();
        assert_eq!(renamed.rewritten, std::slice::from_ref(&good));
        assert_eq!(renamed.failed.len(), 1, "{:?}", renamed.failed);
        assert_eq!(renamed.failed[0].0, newer);
        assert!(
            renamed.failed[0].1.contains("newer"),
            "{:?}",
            renamed.failed
        );
        assert_eq!(text_of(&good), "[fresh]");
    }

    #[test]
    fn renaming_to_a_taken_key_changes_nothing() {
        let notebook = notebook("ref-rename-taken");
        save(&notebook, "old", &article()).unwrap();
        let other = Reference {
            title: "Something else".to_string(),
            ..article()
        };
        save(&notebook, "Taken", &other).unwrap();
        let doc = notebook.root.join("one.kame");
        write_document(&doc, "[old]", &[]);
        for new in [
            "Taken", "taken", "TAKEN", "old", "OLD", "1bad", "", "no way",
        ] {
            assert!(rename(&notebook, "old", new, None).is_err(), "{new:?}");
        }
        assert!(rename(&notebook, "missing", "fresh", None).is_err());
        assert_eq!(text_of(&doc), "[old]");
        let names: Vec<String> = stamp(&notebook).into_iter().map(|(n, ..)| n).collect();
        assert_eq!(names, ["Taken.yaml", "old.yaml"]);
    }

    #[test]
    fn a_renamed_document_in_a_logged_folder_keeps_the_rename_after_it_syncs() {
        let notebook = notebook("ref-rename-logged");
        std::fs::write(notebook.root.join(kame_sync::logs::MARKER), "").unwrap();
        save(&notebook, "old", &article()).unwrap();
        let doc = notebook.root.join("one.kame");
        write_document(&doc, "see [old]", &[]);
        // It has synced before, so the logs already hold the text with `[old]`.
        let sync = |clock| {
            let mut session = kame_core::session::Session::open(&doc).unwrap();
            let mut logged = kame_sync::logs::Logged::open(&session, "talos-aaaa").unwrap();
            logged.sync(&mut session, clock).unwrap();
        };
        sync(1_000);
        let renamed = rename(&notebook, "old", "fresh", None).unwrap();
        assert_eq!(renamed.rewritten, std::slice::from_ref(&doc));
        sync(2_000);
        assert_eq!(text_of(&doc), "see [fresh]");
        let store = kame_sync::revlog::Store::load(&kame_sync::logs::log_folder(&doc)).unwrap();
        let head = store.heads().into_iter().next().unwrap();
        assert_eq!(store.document(&head).blocks[0].text, "see [fresh]");
    }

    #[test]
    fn renaming_a_document_with_only_logs_builds_it_and_rewrites_it() {
        let notebook = notebook("ref-rename-log-only");
        std::fs::write(notebook.root.join(kame_sync::logs::MARKER), "").unwrap();
        save(&notebook, "old", &article()).unwrap();
        let doc = notebook.root.join("one.kame");
        write_document(&doc, "see [old]", &[]);
        {
            let mut session = kame_core::session::Session::open(&doc).unwrap();
            let mut logged = kame_sync::logs::Logged::open(&session, "talos-aaaa").unwrap();
            logged.sync(&mut session, 1_000).unwrap();
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", doc.display()));
        }
        assert!(!doc.exists());
        assert_eq!(citing(&notebook)["old"], std::slice::from_ref(&doc));
        let renamed = rename(&notebook, "old", "fresh", None).unwrap();
        assert_eq!(renamed.failed, []);
        assert_eq!(renamed.rewritten, std::slice::from_ref(&doc));
        assert_eq!(text_of(&doc), "see [fresh]");
        assert_eq!(read_document(&doc).unwrap().0.blocks.len(), 1);
    }

    #[test]
    fn renaming_names_an_unreadable_document_and_keeps_the_old_reference() {
        let notebook = notebook("ref-rename-unreadable");
        save(&notebook, "old", &article()).unwrap();
        let bad = notebook.root.join("bad.kame");
        std::fs::write(&bad, b"not a database at all").unwrap();
        let good = notebook.root.join("good.kame");
        write_document(&good, "[old]", &[]);
        let renamed = rename(&notebook, "old", "fresh", None).unwrap();
        assert_eq!(renamed.rewritten, std::slice::from_ref(&good));
        assert_eq!(renamed.failed.len(), 1, "{:?}", renamed.failed);
        assert_eq!(renamed.failed[0].0, bad);
        assert!(
            renamed.failed[0].1.contains("may cite"),
            "{:?}",
            renamed.failed
        );
        assert!(renamed.old_kept);
        assert!(folder(&notebook).join("old.yaml").exists());
        assert!(folder(&notebook).join("fresh.yaml").exists());
        assert_eq!(std::fs::read(&bad).unwrap(), b"not a database at all");
    }

    #[cfg(unix)]
    #[test]
    fn renaming_skips_the_open_document_given_by_a_linked_path() {
        let notebook = notebook("ref-rename-link");
        save(&notebook, "old", &article()).unwrap();
        let doc = notebook.root.join("open.kame");
        write_document(&doc, "[old]", &[]);
        let elsewhere = kame_test_dir::folder("ref-rename-link-to");
        let link = elsewhere.join("alias.kame");
        std::os::unix::fs::symlink(&doc, &link).unwrap();
        let renamed = rename(&notebook, "old", "fresh", Some(&link)).unwrap();
        assert_eq!(renamed.rewritten, Vec::<PathBuf>::new());
        assert_eq!(renamed.failed, []);
        assert_eq!(text_of(&doc), "[old]");
    }

    #[test]
    fn a_rename_that_left_documents_can_be_run_again() {
        let notebook = notebook("ref-rename-retry");
        save(&notebook, "old", &article()).unwrap();
        let md = notebook.root.join("a.md");
        std::fs::write(&md, "Cites [old].\n").unwrap();
        let kame = notebook.root.join("b.kame");
        write_document(&kame, "[old]", &[]);
        let first = rename(&notebook, "old", "fresh", None).unwrap();
        assert!(first.old_kept);
        assert_eq!(first.failed.len(), 1);
        // The person fixes the Markdown document by hand and tries again.
        std::fs::write(&md, "Cites [fresh].\n").unwrap();
        let second = rename(&notebook, "old", "fresh", None).unwrap();
        assert_eq!(second.failed, []);
        assert_eq!(second.rewritten, Vec::<PathBuf>::new());
        assert!(!second.old_kept);
        assert!(!folder(&notebook).join("old.yaml").exists());
        assert_eq!(load(&notebook).references.get("fresh"), Some(&article()));
        assert_eq!(text_of(&kame), "[fresh]");
    }

    #[test]
    fn a_key_that_synced_away_can_be_added_again_and_renamed() {
        let notebook = notebook("ref-rename-add-again");
        // What another machine has: the new file, and a document still citing the old key.
        save(&notebook, "fresh", &article()).unwrap();
        let doc = notebook.root.join("one.kame");
        write_document(&doc, "see [old]", &[]);
        assert_eq!(
            rename(&notebook, "old", "fresh", None),
            Err("there is no reference \"old\"".to_string())
        );

        // With other contents it could be another source, so it's refused.
        let other = Reference {
            title: "Something else".to_string(),
            ..article()
        };
        save(&notebook, "old", &other).unwrap();
        assert!(rename(&notebook, "old", "fresh", None).is_err());
        assert_eq!(text_of(&doc), "see [old]");

        save(&notebook, "old", &article()).unwrap();
        let renamed = rename(&notebook, "old", "fresh", None).unwrap();
        assert_eq!(renamed.failed, []);
        assert_eq!(renamed.rewritten, std::slice::from_ref(&doc));
        assert!(!renamed.old_kept);
        assert_eq!(text_of(&doc), "see [fresh]");
        assert!(!folder(&notebook).join("old.yaml").exists());
        assert_eq!(load(&notebook).references.get("fresh"), Some(&article()));
    }

    #[cfg(unix)]
    #[test]
    fn a_rename_that_cant_remove_the_old_file_keeps_what_it_did() {
        use std::os::unix::fs::PermissionsExt;
        let notebook = notebook("ref-rename-stuck");
        save(&notebook, "old", &article()).unwrap();
        save(&notebook, "fresh", &article()).unwrap();
        let doc = notebook.root.join("one.kame");
        write_document(&doc, "[old]", &[]);
        let references = folder(&notebook);
        std::fs::set_permissions(&references, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = rename(&notebook, "old", "fresh", None);
        std::fs::set_permissions(&references, std::fs::Permissions::from_mode(0o755)).unwrap();
        let renamed = result.unwrap();
        assert_eq!(renamed.rewritten, std::slice::from_ref(&doc));
        assert!(renamed.old_kept);
        assert!(references.join("old.yaml").exists());
        assert_eq!(text_of(&doc), "[fresh]");
    }
}
