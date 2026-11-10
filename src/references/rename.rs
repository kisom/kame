//! A rename of a key, run on a background thread over the other documents, and finished here
//! over the open one.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use kame_core::lock::Locks;
use kame_core::references::{cited, Reference, References};
use kame_notebook::references::{remove, rename, Open, Renamed};
use kame_notebook::Notebook;

use crate::KameApp;

use super::list::documents;

/// A rename a background thread is doing, leaving the open document for when it's done.
pub(super) struct Renaming {
    pub(super) coming: Receiver<Result<Renamed, String>>,
    /// The notebook it's in, which is the one to remove the old file from.
    pub(super) notebook: Notebook,
    /// The document it left alone, for this app to rewrite, and the old file with it.
    pub(super) open: Option<PathBuf>,
    pub(super) old: String,
    pub(super) new: String,
}

/// What the References… window says of a rename, naming documents from `root`.
fn rename_outcome(old: &str, new: &str, renamed: &Renamed, root: &Path) -> Vec<String> {
    let mut lines = vec![format!(
        "Renamed {old} to {new} in {}.",
        documents(renamed.rewritten.len())
    )];
    if renamed.old_kept {
        lines.push(format!(
            "{old} was kept, so that [{old}] still resolves. Renaming it again once this is fixed \
             finishes it."
        ));
    }
    for (path, why) in &renamed.failed {
        let shown = path.strip_prefix(root).unwrap_or(path);
        lines.push(format!("{}: {why}", shown.display()));
    }
    lines
}

impl KameApp {
    /// Says so and returns true if a rename is running. It has left the open document to be
    /// rewritten when it's done, which needs that document to still be the open one.
    pub(crate) fn held_by_rename(&mut self) -> bool {
        let held = self.refs.renaming.is_some();
        if held {
            self.notice_tracker.announced();
            self.notice = Some("A reference is being renamed. Try again when it's done.".into());
        }
        held
    }

    pub(super) fn start_rename(&mut self, old: String, new: String) {
        let Some(notebook) = self.nb.notebook.clone() else {
            return;
        };
        let open = self.session.path().map(Path::to_path_buf);
        self.ensure_device();
        let device = self.device.clone();
        let now = crate::now_unix();
        let (send, coming) = channel();
        self.refs.renaming = Some(Renaming {
            coming,
            notebook: notebook.clone(),
            open: open.clone(),
            old: old.clone(),
            new: new.clone(),
        });
        // The open document is left out, so this window holds nothing the rename writes into.
        let folder = crate::locks_folder();
        std::thread::spawn(move || {
            // A document with no file can hold `[old]` too, so the old file is always left here.
            let leaving = open.as_deref().map_or(Open::Unsaved, Open::File);
            let locks = Locks::new(folder.as_deref(), None);
            let _ = send.send(rename(&notebook, &old, &new, leaving, locks, &device, now));
        });
    }

    /// The rename has done the other documents, and left the old file for the open document. This
    /// does the open document, last because `rename` can refuse to start, and a refused rename
    /// should have touched nothing. The old file goes once everything has followed, so that a quit
    /// before now leaves `[old]` with its reference.
    pub(super) fn finish_rename(&mut self, result: Result<Renamed, String>) {
        let Some(renaming) = self.refs.renaming.take() else {
            return;
        };
        let Renaming {
            notebook,
            open,
            old,
            new,
            ..
        } = renaming;
        let mut renamed = match result {
            Ok(renamed) => renamed,
            Err(refused) => {
                self.say_in_references(vec![refused]);
                return;
            }
        };
        // Left alone for being open, it's no longer what's open if it was saved as another file.
        if let Some(left) =
            open.filter(|open| !crate::same_document(self.session.path(), Some(open)))
        {
            let why = "it was saved as another file during this, so it may still cite this";
            renamed.failed.push((left, why.to_string()));
        }
        // An import whose Markdown file couldn't be rewritten stays as that file is, keeping the
        // words the report names, and the old key is kept for it.
        let source = self.session.source_path.as_deref();
        let source_failed = self.session.is_anonymous()
            && renamed
                .failed
                .iter()
                .any(|(path, _)| crate::same_document(source, Some(path)));
        if !source_failed {
            let here = self
                .session
                .path()
                .map_or_else(|| PathBuf::from("this document"), Path::to_path_buf);
            if !self.take_in_from_logs() {
                let why = "what other machines wrote to it couldn't be taken in, so it may still \
                           cite this";
                renamed.failed.push((here.clone(), why.to_string()));
            }
            match self.rename_in_open_document(&old, &new) {
                Ok(true) => renamed.rewritten.push(here),
                Ok(false) => {}
                Err(why) => renamed.failed.push((here, why)),
            }
        }
        let mut not_removed = None;
        if renamed.failed.is_empty() {
            match remove(&notebook, &old) {
                Ok(()) => renamed.old_kept = false,
                Err(why) => not_removed = Some(format!("{old} wasn't removed: {why}")),
            }
        }
        let mut said = rename_outcome(&old, &new, &renamed, &notebook.root);
        said.extend(not_removed);
        self.say_in_references(said);
        self.reload_references();
    }

    /// Merges what has arrived in the open document's logs, as leaving it does, so that a citation
    /// another machine wrote is in it. Whether nothing is left there to merge, which is so for a
    /// document whose folder doesn't sync, and for a new one nobody has written in, which has no
    /// logs yet.
    fn take_in_from_logs(&mut self) -> bool {
        let Some(path) = self.session.path().map(Path::to_path_buf) else {
            return true;
        };
        if kame_sync::logs::logging_root(&path).is_none() || self.on_untouched_fresh() {
            return true;
        }
        self.set_open_note(None);
        self.sync_logs();
        self.logged
            .as_ref()
            .is_some_and(|logged| !logged.has_news())
    }

    /// Renames `old` in the open document, returning whether it cited it, or why it still does.
    fn rename_in_open_document(&mut self, old: &str, new: &str) -> Result<bool, String> {
        let mut only_old = References::new();
        only_old.insert(old.to_string(), Reference::default());
        let cites = !cited(self.session.document(), &only_old, true).is_empty();
        // Said as for a closed one, and what this kame can't read of it may cite the key.
        if self.session.newer {
            let why = if cites {
                kame_notebook::manage::NEWER
            } else {
                kame_notebook::references::NEWER_MAY_CITE
            };
            return Err(why.to_string());
        }
        if !cites {
            return Ok(false);
        }
        // The rename can be in memory with nothing written, which returns no count.
        let count = self.session.rename_citation(old, new);
        if self.session.save_failed {
            let why = self.session.last_error.clone();
            return Err(why.unwrap_or_else(|| "it could not be saved".to_string()));
        }
        if count == 0 {
            return Err("its citations could not be rewritten".to_string());
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::test_support::*;
    use crate::{test_temp_dir, Headless};
    use kame_core::session::{Opening, Session};

    #[test]
    fn a_renames_outcome_says_what_it_did_and_what_it_left() {
        let root = std::path::Path::new("/notebook");
        let finished = Renamed {
            rewritten: vec![root.join("a.kame"), root.join("b.kame")],
            failed: Vec::new(),
            old_kept: false,
        };
        assert_eq!(
            rename_outcome("old", "new", &finished, root),
            ["Renamed old to new in 2 documents."]
        );

        let stuck = Renamed {
            rewritten: vec![root.join("a.kame")],
            failed: vec![(root.join("sub/b.md"), "it can't be written".to_string())],
            old_kept: true,
        };
        let lines = rename_outcome("old", "new", &stuck, root);
        assert_eq!(lines[0], "Renamed old to new in 1 document.");
        assert!(lines[1].contains("old was kept"), "{lines:?}");
        assert!(
            lines.contains(&"sub/b.md: it can't be written".to_string()),
            "{lines:?}"
        );

        let unremoved = Renamed {
            rewritten: Vec::new(),
            failed: Vec::new(),
            old_kept: true,
        };
        let lines = rename_outcome("old", "new", &unremoved, root);
        assert!(lines[1].contains("old was kept"), "{lines:?}");
    }

    fn text_on_disk(path: &Path) -> String {
        kame_notebook::read_document(path).unwrap().0.blocks[0]
            .text
            .clone()
    }

    /// Renames the only key to `new`, by way of its field and Enter.
    fn rename_only_key(page: &mut Headless, new: &str) {
        ask_to_rename(page, new);
        press_key(page, egui::Key::Enter);
        wait_for_rename(page);
    }

    /// The open document from a newer kame isn't renamed in, on the screen or on disk, and is named
    /// as a closed one is, whether or not what this kame can read of it cites the key.
    #[test]
    fn renaming_leaves_an_open_document_from_a_newer_kame_as_it_is_and_says_why() {
        for (text, why) in [
            (
                "see [knuth1981]",
                "it was written by a newer version of kame; update kame to change it",
            ),
            (
                "nothing cited",
                "it was written by a newer version of kame, so it may cite this in parts this \
                 kame can't read; update kame to change it",
            ),
        ] {
            let mut page = page_in_notebook(text);
            let path = page.app.session.path().unwrap().to_path_buf();
            page.app.session.mark_newer();
            let bytes = std::fs::read(&path).unwrap();
            open_list(&mut page);

            rename_only_key(&mut page, "plass1981");

            assert_eq!(only_text(&page), text, "the screen is as it was");
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert!(reference_file(&page, "knuth1981").exists(), "{text}");
            assert!(page.sees(&format!("doc.kame: {why}")), "{text}");
        }
    }

    #[test]
    fn renaming_rewrites_the_open_document_and_the_others_and_shows_at_once() {
        let mut page = page_in_notebook("see [knuth1981]");
        let other = other_document(&page, "other.kame", "and [knuth1981]");
        open_list(&mut page);

        rename_only_key(&mut page, "plass1981");

        assert_eq!(only_text(&page), "see [plass1981]");
        assert_eq!(text_on_disk(&other), "and [plass1981]");
        assert!(page.app.refs.list.contains("plass1981"));
        assert!(!page.app.refs.list.contains("knuth1981"));
        assert!(!reference_file(&page, "knuth1981").exists());
        assert!(page.sees("Renamed knuth1981 to plass1981 in 2 documents."));
        // The open document's rename is one undo, in that document.
        assert!(page.app.session.undo());
        assert_eq!(only_text(&page), "see [knuth1981]");
    }

    #[test]
    fn a_rename_refused_for_a_taken_key_says_why_and_touches_nothing() {
        let mut page = page_in_notebook("see [knuth1981]");
        write_reference(
            &root_of(&page),
            "taken2000",
            "kind: book\ntitle: Different\n",
        );
        page.app.reload_references();
        open_list(&mut page);
        let history = page.app.session.history();

        page.app
            .start_rename("knuth1981".to_string(), "taken2000".to_string());
        run_until(&mut page, |app| app.refs.renaming.is_none());
        page.frame(Vec::new());

        assert!(page.sees("\"taken2000\" is already a reference"));
        assert_eq!(only_text(&page), "see [knuth1981]");
        assert_eq!(page.app.session.history(), history);
        assert!(reference_file(&page, "knuth1981").exists());
    }

    #[test]
    fn a_document_that_cant_be_rewritten_keeps_the_old_key_and_is_named() {
        let mut page = page_in_notebook("see [knuth1981]");
        std::fs::write(root_of(&page).join("a.md"), "Cites [knuth1981].\n").unwrap();
        open_list(&mut page);

        rename_only_key(&mut page, "plass1981");

        assert_eq!(only_text(&page), "see [plass1981]");
        assert!(reference_file(&page, "knuth1981").exists());
        assert!(reference_file(&page, "plass1981").exists());
        assert!(page.app.refs.list.contains("knuth1981"));
        assert!(page.sees("knuth1981 was kept"));
        assert!(page.sees("a.md: a Markdown document"));
    }

    #[test]
    fn when_the_open_document_cant_follow_the_old_key_stays_and_a_retry_finishes() {
        let mut page = page_in_notebook("see [knuth1981]");
        let other = other_document(&page, "other.kame", "and [knuth1981]");
        let was = std::fs::read(reference_file(&page, "knuth1981")).unwrap();
        open_list(&mut page);
        page.app.session.save_failed = true;
        page.app.session.last_error = Some("the disk is full".to_string());

        rename_only_key(&mut page, "plass1981");

        // Never removed, as the open document didn't follow.
        assert_eq!(
            std::fs::read(reference_file(&page, "knuth1981")).unwrap(),
            was
        );
        assert!(reference_file(&page, "plass1981").exists());
        assert!(page.app.refs.list.contains("knuth1981"));
        assert_eq!(text_on_disk(&other), "and [plass1981]");
        assert_eq!(only_text(&page), "see [knuth1981]");
        assert!(page.sees("knuth1981 was kept"));
        assert!(page.sees("doc.kame: the disk is full"));

        page.app.session.save_failed = false;
        page.app
            .start_rename("knuth1981".to_string(), "plass1981".to_string());
        run_until(&mut page, |app| app.refs.renaming.is_none());

        assert_eq!(only_text(&page), "see [plass1981]");
        assert!(!reference_file(&page, "knuth1981").exists());
        assert!(!page.app.refs.list.contains("knuth1981"));
    }

    #[test]
    fn the_old_file_stays_until_the_open_document_has_followed() {
        let mut page = page_in_notebook("see [knuth1981]");
        let notebook = page.app.nb.notebook.clone().unwrap();
        let open = page.app.session.path().unwrap().to_path_buf();
        open_list(&mut page);
        let send = rename_held_open(&mut page);

        let done = rename(
            &notebook,
            "knuth1981",
            "plass1981",
            Open::File(&open),
            Locks::NONE,
            "here-aaaa",
            1_000,
        );

        // A quit here leaves the open document with a reference for what it holds.
        assert_eq!(text_on_disk(&open), "see [knuth1981]");
        assert!(reference_file(&page, "knuth1981").exists());

        send.send(done).unwrap();
        run_until(&mut page, |app| app.refs.renaming.is_none());
        page.frame(Vec::new());

        assert_eq!(only_text(&page), "see [plass1981]");
        assert!(!reference_file(&page, "knuth1981").exists());
        assert!(!page.app.refs.list.contains("knuth1981"));
        assert!(page.sees("Renamed knuth1981 to plass1981 in 1 document."));
        assert!(!page.sees("was kept"));
    }

    /// Whether a folder with no write permission turns away a new file. It doesn't for root, which
    /// a test that needs a refusal has to skip.
    #[cfg(unix)]
    fn read_only_folder_refuses_files(folder: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o555)).unwrap();
        let probe = folder.join("probe");
        let refused = std::fs::File::create(&probe).is_err();
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_file(&probe);
        if !refused {
            eprintln!("skipped: this user can write to a read-only folder");
        }
        refused
    }

    #[cfg(unix)]
    #[test]
    fn an_old_file_that_cant_be_removed_at_the_end_is_named_and_a_retry_finishes() {
        use std::os::unix::fs::PermissionsExt;
        let mut page = page_in_notebook("see [knuth1981]");
        let notebook = page.app.nb.notebook.clone().unwrap();
        let open = page.app.session.path().unwrap().to_path_buf();
        open_list(&mut page);
        let send = rename_held_open(&mut page);
        let done = rename(
            &notebook,
            "knuth1981",
            "plass1981",
            Open::File(&open),
            Locks::NONE,
            "here-aaaa",
            1_000,
        );
        let folder = root_of(&page).join("references");
        if !read_only_folder_refuses_files(&folder) {
            return;
        }

        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
        send.send(done).unwrap();
        run_until(&mut page, |app| app.refs.renaming.is_none());
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        page.frame(Vec::new());

        assert_eq!(only_text(&page), "see [plass1981]");
        assert!(reference_file(&page, "knuth1981").exists());
        assert!(page.sees("knuth1981 was kept"));
        assert!(page.sees("knuth1981 wasn't removed"));

        page.app
            .start_rename("knuth1981".to_string(), "plass1981".to_string());
        run_until(&mut page, |app| app.refs.renaming.is_none());
        assert!(!reference_file(&page, "knuth1981").exists());
    }

    /// A page in a notebook holding `knuth1981`, with a Markdown file of `on_disk` imported.
    fn imported_markdown(on_disk: &str) -> Headless {
        let mut page = page_in_notebook("");
        let markdown = root_of(&page).join("a.md");
        std::fs::write(&markdown, on_disk).unwrap();
        let (imported, _) = Session::import(&markdown).unwrap();
        page.app.adopt_session(imported);
        page.frame(Vec::new());
        assert!(page.app.session.is_anonymous());
        page
    }

    #[test]
    fn an_imported_copy_keeps_the_key_when_its_markdown_file_cant_be_rewritten() {
        let mut page = imported_markdown("Cites [knuth1981].\n");
        let history = page.app.session.history();
        open_list(&mut page);

        rename_only_key(&mut page, "plass1981");

        // One account of those words: not renamed, because the file can't be.
        assert_eq!(only_text(&page), "Cites [knuth1981].");
        assert_eq!(page.app.session.history(), history);
        assert_eq!(
            std::fs::read_to_string(root_of(&page).join("a.md")).unwrap(),
            "Cites [knuth1981].\n"
        );
        assert!(page.sees("Renamed knuth1981 to plass1981 in 0 documents."));
        assert!(page.sees("knuth1981 was kept"));
        assert!(page.sees("a.md: a Markdown document"));
        assert!(page.app.refs.list.contains("knuth1981"));
    }

    #[test]
    fn an_imported_copy_follows_when_its_file_doesnt_cite_the_key() {
        let mut page = imported_markdown("No citation on disk.\n");
        page.app.session.insert(" [knuth1981]");
        open_list(&mut page);

        rename_only_key(&mut page, "plass1981");

        assert!(
            only_text(&page).contains("[plass1981]"),
            "{}",
            only_text(&page)
        );
        assert!(!reference_file(&page, "knuth1981").exists());
        assert!(page.sees("Renamed knuth1981 to plass1981 in 1 document."));
    }

    #[test]
    fn an_unsaved_copy_that_cant_follow_keeps_the_old_key() {
        let mut page = imported_markdown("No citation on disk.\n");
        page.app.session.insert(" [knuth1981]");
        open_list(&mut page);
        page.app.session.save_failed = true;
        page.app.session.last_error = Some("the disk is full".to_string());

        rename_only_key(&mut page, "plass1981");

        assert!(reference_file(&page, "knuth1981").exists());
        assert!(reference_file(&page, "plass1981").exists());
        assert!(page.app.refs.list.contains("knuth1981"));
        assert!(page.sees("this document: the disk is full"));
        assert!(page.sees("knuth1981 was kept"));
    }

    #[test]
    fn an_open_document_from_a_newer_version_keeps_the_old_key_too() {
        let mut page = page_in_notebook("see [knuth1981]");
        open_list(&mut page);
        page.app.session.newer = true;

        rename_only_key(&mut page, "plass1981");

        assert!(reference_file(&page, "knuth1981").exists());
        assert_eq!(only_text(&page), "see [knuth1981]");
        assert!(page
            .sees("doc.kame: it was written by a newer version of kame; update kame to change it"));
    }

    #[test]
    fn an_open_document_that_doesnt_cite_it_is_no_failure() {
        let mut page = page_in_notebook("no citations here");
        let other = other_document(&page, "other.kame", "and [knuth1981]");
        open_list(&mut page);
        page.app.session.save_failed = true;

        rename_only_key(&mut page, "plass1981");

        assert_eq!(text_on_disk(&other), "and [plass1981]");
        assert!(!reference_file(&page, "knuth1981").exists());
        assert!(page.sees("in 1 document."));
    }

    #[test]
    fn the_document_stays_open_while_a_rename_runs() {
        let mut page = page_in_notebook("[knuth1981]");
        let other = other_document(&page, "other.kame", "words");
        open_list(&mut page);
        let (send, coming) = channel();
        page.app.refs.renaming = Some(Renaming {
            coming,
            notebook: page.app.nb.notebook.clone().unwrap(),
            open: page.app.session.path().map(Path::to_path_buf),
            old: "knuth1981".to_string(),
            new: "plass1981".to_string(),
        });
        let open = page.app.session.path().unwrap().to_path_buf();

        page.app
            .open_document(other.display().to_string(), Opening::MayCreate);
        assert_eq!(page.app.session.path(), Some(open.as_path()));
        assert!(page
            .app
            .notice
            .as_deref()
            .unwrap()
            .contains("being renamed"));

        // Nor does Escape take away the window that will say how it went.
        press_key(&mut page, egui::Key::Escape);
        assert!(page.app.refs.window.is_some());
        page.frame(Vec::new());
        assert!(page.sees("Renaming…"));

        send.send(Err("refused".to_string())).unwrap();
        run_until(&mut page, |app| app.refs.renaming.is_none());
        page.app
            .open_document(other.display().to_string(), Opening::MayCreate);
        assert_eq!(page.app.session.path(), Some(other.as_path()));
        assert!(
            page.app.refs.window.is_none(),
            "another document's windows are its own"
        );
    }

    #[test]
    fn save_as_waits_for_a_rename() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);
        let _send = rename_held_open(&mut page);
        let ctx = page.ctx.clone();

        // A test panics if this asks where to save.
        page.app.save_with_dialog(&ctx, false);

        assert!(page
            .app
            .notice
            .as_deref()
            .unwrap()
            .contains("being renamed"));
    }

    #[test]
    fn a_document_saved_elsewhere_during_a_rename_keeps_the_old_key_and_is_named() {
        let mut page = page_in_notebook("see [knuth1981]");
        let original = page.app.session.path().unwrap().to_path_buf();
        open_list(&mut page);
        page.app
            .start_rename("knuth1981".to_string(), "plass1981".to_string());
        page.app
            .session
            .save_as(&root_of(&page).join("copy.kame"), None, |_, _| false)
            .unwrap();

        run_until(&mut page, |app| app.refs.renaming.is_none());
        page.frame(Vec::new());

        assert_eq!(text_on_disk(&original), "see [knuth1981]");
        assert!(
            reference_file(&page, "knuth1981").exists(),
            "[old] still resolves"
        );
        assert!(page.sees("knuth1981 was kept"));
        assert!(page.sees("doc.kame: "), "the document left behind is named");
    }

    /// A page on `doc.kame` saying `see`, in a notebook whose folder syncs through logs, with the
    /// reference `knuth1981`, synced once.
    fn page_in_a_logged_notebook() -> Headless {
        let root = notebook_folder("logged").canonicalize().unwrap();
        std::fs::write(root.join(kame_sync::logs::MARKER), "").unwrap();
        write_reference(&root, "knuth1981", GOOD);
        let mut session = Session::open(&root.join("doc.kame")).unwrap();
        session.insert("see");
        let mut page = Headless::new(session);
        page.app.device = "titan-bbbb".to_string();
        page.read_references();
        page.app.sync_logs();
        page
    }

    /// Writes ` and [knuth1981]` to the end of the open document on another machine, whose log
    /// then arrives here, with nothing merged.
    fn cite_from_another_machine(page: &Headless) {
        use kame_sync::logs::{build_from_logs, log_folder, Logged, MARKER};
        let copy_logs = |from: &Path, to: &Path| {
            let (from, to) = (log_folder(from), log_folder(to));
            std::fs::create_dir_all(&to).unwrap();
            for entry in std::fs::read_dir(from).unwrap().flatten() {
                std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
            }
        };
        let here = page.app.session.path().unwrap().to_path_buf();
        let there = test_temp_dir().join(format!("there-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&there).unwrap();
        std::fs::write(there.join(MARKER), "").unwrap();
        let there = there.canonicalize().unwrap().join("doc.kame");
        copy_logs(&here, &there);
        build_from_logs(&there).unwrap();
        let mut elsewhere = Session::open(&there).unwrap();
        let end = elsewhere.document_edge(true).unwrap();
        elsewhere.set_caret(end);
        elsewhere.insert(" and [knuth1981]");
        Logged::open(&elsewhere, "talos-aaaa")
            .unwrap()
            .sync(&mut elsewhere, 200)
            .unwrap();
        copy_logs(&there, &here);
    }

    #[test]
    fn a_rename_takes_in_what_another_machine_cited_in_the_open_document_first() {
        let mut page = page_in_a_logged_notebook();
        cite_from_another_machine(&page);
        assert_eq!(only_text(&page), "see");
        open_list(&mut page);

        rename_only_key(&mut page, "plass1981");

        assert_eq!(only_text(&page), "see and [plass1981]");
        assert!(!reference_file(&page, "knuth1981").exists());
        assert!(page.sees("Renamed knuth1981 to plass1981 in 1 document."));
    }

    #[test]
    fn a_rename_keeps_the_old_key_when_the_open_documents_logs_cant_be_read() {
        let mut page = page_in_a_logged_notebook();
        cite_from_another_machine(&page);
        let logs = kame_sync::logs::log_folder(page.app.session.path().unwrap());
        // A log that is a folder can't be read.
        std::fs::create_dir_all(logs.join("other.log")).unwrap();
        page.app.logged = None;
        open_list(&mut page);

        rename_only_key(&mut page, "plass1981");

        assert!(
            reference_file(&page, "knuth1981").exists(),
            "[old] may yet arrive"
        );
        assert!(page.sees("knuth1981 was kept"));
        assert!(page.sees("doc.kame: "), "the document is named");
    }

    #[test]
    fn leaving_the_notebook_during_a_rename_keeps_its_window_and_outcome() {
        let mut page = page_in_notebook("see [knuth1981]");
        let root = root_of(&page);
        open_list(&mut page);
        let send = rename_held_open(&mut page);
        let outside = test_temp_dir().join(format!("outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside).unwrap();

        page.app
            .session
            .save_as(&outside.join("copy.kame"), None, |_, _| false)
            .unwrap();
        page.frame(Vec::new());
        page.frame(Vec::new());

        assert!(page.app.nb.notebook.is_none());
        assert!(page.app.refs.renaming.is_some(), "still waited for");
        assert!(
            page.app.refs.window.is_some(),
            "still there to say how it went"
        );
        assert!(page.sees("Renaming…"));

        let notebook = Notebook::open(&root).unwrap();
        let open = root.join("doc.kame");
        send.send(rename(
            &notebook,
            "knuth1981",
            "plass1981",
            Open::File(&open),
            Locks::NONE,
            "here-aaaa",
            1_000,
        ))
        .unwrap();
        run_until(&mut page, |app| app.refs.renaming.is_none());
        page.frame(Vec::new());

        assert!(page.sees("knuth1981 was kept"));
        assert!(root.join("references/knuth1981.yaml").exists());
        assert_eq!(text_on_disk(&open), "see [knuth1981]");
    }

    #[test]
    fn a_link_refused_during_a_rename_leaves_the_way_back_as_it_was() {
        let mut page = page_in_notebook("hello");
        other_document(&page, "other.kame", "elsewhere");
        let _send = rename_held_open(&mut page);
        let opened = page.app.session.path().map(Path::to_path_buf);

        page.app.follow_document_link("other.kame");

        assert_eq!(page.app.session.path().map(Path::to_path_buf), opened);
        assert!(page.app.back.stops.is_empty());
    }

    #[test]
    fn a_document_opened_from_a_list_during_a_rename_leaves_the_way_back_as_it_was() {
        let mut page = page_in_notebook("hello");
        let other = other_document(&page, "other.kame", "elsewhere");
        let _send = rename_held_open(&mut page);

        page.app.open_from_notebook(other, None, Opening::MustExist);

        assert!(page.app.back.stops.is_empty());
    }
}
