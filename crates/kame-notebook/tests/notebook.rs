//! A notebook on disk: made-up folders of documents.

use std::path::{Path, PathBuf};

use kame_core::session::Session;
use kame_notebook::list::{title_of, Kind};
use kame_notebook::Notebook;

pub fn folder(name: &str) -> PathBuf {
    let root = std::env::temp_dir()
        .join("kame-notebook-tests")
        .join(format!("{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

fn document(path: &Path, words: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut session = Session::open(path).unwrap();
    session.insert(words);
}

fn notebook(root: &Path) -> Notebook {
    Notebook::open(root).unwrap()
}

#[test]
fn the_nearest_notebook_up_through_the_parents_is_the_one() {
    let root = folder("find");
    std::fs::create_dir_all(root.join("inner/deep")).unwrap();
    let file = root.join("inner/deep/a.kame");
    assert_eq!(Notebook::find(&file).unwrap(), None);
    std::fs::write(root.join("notebook.yaml"), "title: Outer\n").unwrap();
    assert_eq!(Notebook::find(&file).unwrap().unwrap().title(), "Outer");
    std::fs::write(root.join("inner/notebook.yaml"), "title: Inner\n").unwrap();
    assert_eq!(Notebook::find(&file).unwrap().unwrap().title(), "Inner");
    let found = Notebook::find(&root.join("inner")).unwrap().unwrap();
    assert_eq!(found.title(), "Inner", "a folder can be named too");
}

#[test]
fn settings_are_optional_and_a_typo_is_refused() {
    let root = folder("Philosophy");
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    let named = notebook(&root).title();
    assert!(named.starts_with("Philosophy-"), "{named}");
    std::fs::write(
        root.join("notebook.yaml"),
        "title: T\nurl: https://x.example/\nauthor: A\nremote: h:/p/\n",
    )
    .unwrap();
    let settings = notebook(&root).settings;
    assert_eq!(settings.url.as_deref(), Some("https://x.example/"));
    assert_eq!(settings.remote.as_deref(), Some("h:/p/"));
    std::fs::write(root.join("notebook.yaml"), "titel: T\n").unwrap();
    let error = Notebook::open(&root).unwrap_err();
    assert!(error.contains("titel"), "{error}");
}

#[test]
fn the_list_holds_documents_and_files_and_leaves_out_how_things_work() {
    let root = folder("list");
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    document(&root.join("Notebook.kame"), "Contents.");
    document(&root.join("maritain/materialism.kame"), "Words.");
    std::fs::write(root.join("about.md"), "# About\n\nWords.\n").unwrap();
    std::fs::create_dir_all(root.join("static")).unwrap();
    std::fs::write(root.join("static/paper.pdf"), "pdf").unwrap();
    std::fs::write(root.join("static/notes.md"), "not a document here").unwrap();
    std::fs::create_dir_all(root.join("theme")).unwrap();
    std::fs::write(root.join("theme/page.kame"), "not a document here").unwrap();
    std::fs::create_dir_all(root.join("papers")).unwrap();
    std::fs::write(root.join("papers/private.pdf"), "pdf").unwrap();
    std::fs::create_dir_all(root.join(".hidden")).unwrap();
    std::fs::write(root.join(".hidden/secret.kame"), "x").unwrap();
    std::fs::write(root.join(".kame-logs"), "").unwrap();
    std::fs::write(
        root.join("about.sync-conflict-20260101-120000-ABC.kame"),
        "x",
    )
    .unwrap();
    std::fs::write(root.join("old.kame.merged"), "x").unwrap();
    std::fs::write(root.join("a.kame-wal"), "x").unwrap();

    let found = notebook(&root);
    let documents: Vec<String> = found
        .documents()
        .into_iter()
        .map(|entry| entry.relative)
        .collect();
    assert_eq!(
        documents,
        ["Notebook.kame", "about.md", "maritain/materialism.kame"]
    );
    let files: Vec<String> = found
        .entries()
        .into_iter()
        .filter(|entry| entry.kind == Kind::File)
        .map(|entry| entry.relative)
        .collect();
    assert_eq!(
        files,
        [
            "papers/private.pdf",
            "static/notes.md",
            "static/paper.pdf",
            "theme/page.kame"
        ]
    );
}

#[test]
fn a_document_with_only_logs_is_listed_and_read_from_them() {
    let root = folder("logs");
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    std::fs::write(root.join(".kame-logs"), "").unwrap();
    let file = root.join("essay.kame");
    {
        let mut session = Session::open(&file).unwrap();
        session.insert("From the logs.");
        let mut metadata = session.metadata.clone();
        metadata.title = "Logged".to_string();
        session.set_metadata(metadata);
        kame_sync::logs::Logged::open(&session, "talos-aaaa")
            .unwrap()
            .sync(&mut session, 1)
            .unwrap();
    }
    let found = notebook(&root);
    assert_eq!(found.documents().len(), 1, "logs don't list it twice");
    std::fs::remove_file(&file).unwrap();
    let documents = found.documents();
    assert_eq!(documents.len(), 1, "they stand for it once it's gone");
    assert_eq!(documents[0].path, file);
    assert_eq!(title_of(&file), "Logged");
    assert!(!file.exists(), "reading it doesn't build it");
}

#[test]
fn a_symbolic_link_out_of_the_notebook_is_not_followed() {
    let root = folder("symlink");
    let outside = folder("outside");
    document(&outside.join("private.kame"), "Secret.");
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("away")).unwrap();
    assert!(notebook(&root).entries().is_empty());
}

#[test]
fn a_title_comes_from_properties_then_the_document_then_the_file() {
    let root = folder("titles");
    let file = root.join("plain.kame");
    document(&file, "Words.");
    assert_eq!(title_of(&file), "plain");
    {
        let mut session = Session::open(&file).unwrap();
        let mut metadata = session.metadata.clone();
        metadata.title = "Named".to_string();
        session.set_metadata(metadata);
    }
    assert_eq!(title_of(&file), "Named");
    std::fs::write(
        root.join("page.md"),
        "---\ntitle: From Front Matter\n---\n\nWords.\n",
    )
    .unwrap();
    assert_eq!(title_of(&root.join("page.md")), "From Front Matter");
    assert_eq!(title_of(&root.join("gone.kame")), "gone");
}

mod searching {
    use super::*;
    use kame_core::edit::Cursor;
    use kame_core::model::AnnotationKind;
    use kame_notebook::search::{search, Hit};
    use std::sync::atomic::AtomicBool;

    fn hits(found: &Notebook, needle: &str) -> Vec<Hit> {
        let mut all = Vec::new();
        let cancel = AtomicBool::new(false);
        search(found, needle, None, &cancel, |hit| all.push(hit));
        all
    }

    #[test]
    fn it_matches_as_ctrl_f_does_across_every_document() {
        let root = folder("search");
        std::fs::write(root.join("notebook.yaml"), "").unwrap();
        document(&root.join("a.kame"), "The MATERIAL world.");
        document(&root.join("deep/b.kame"), "Immaterial and material—both.");
        std::fs::write(root.join("c.md"), "# C\n\nNothing here.\n").unwrap();
        std::fs::create_dir_all(root.join("static")).unwrap();
        std::fs::write(root.join("static/d.md"), "material in a paper").unwrap();
        let found = notebook(&root);
        let all = hits(&found, "terial");
        assert_eq!(
            all.len(),
            3,
            "any run of letters, ignoring case, not static/"
        );
        assert_eq!(all[0].relative, "a.kame");
        assert_eq!(&all[0].line, "The MATERIAL world.");
        assert_eq!(hits(&found, "—").len(), 1);
        assert_eq!(hits(&found, "e").len() > 3, true);
        assert!(hits(&found, "").is_empty());
        assert!(hits(&found, "nowhere at all").is_empty());
    }

    #[test]
    fn it_reads_margin_notes_footnotes_and_private_notes() {
        let root = folder("searchnotes");
        std::fs::write(root.join("notebook.yaml"), "").unwrap();
        let file = root.join("a.kame");
        {
            let mut session = Session::open(&file).unwrap();
            session.insert("Words to hang things on.");
            let block_id = session.document().blocks[0].id;
            for (kind, body) in [
                (AnnotationKind::Marginal, "marginword"),
                (AnnotationKind::Footnote, "footword"),
                (AnnotationKind::Note, "privateword"),
            ] {
                session.set_caret(Cursor {
                    block_id,
                    offset: 6,
                });
                session.select_to(Cursor {
                    block_id,
                    offset: 8,
                });
                let added = session.add_annotation(kind).unwrap();
                session.set_annotation_body(added, body);
            }
        }
        let found = notebook(&root);
        for word in ["marginword", "footword", "privateword"] {
            let all = hits(&found, word);
            assert_eq!(all.len(), 1, "{word}");
            // A margin note hangs on its whole block, and the others on their words.
            let want = if word == "marginword" { 0 } else { 6 };
            assert_eq!(all[0].offset, want, "{word} leads to where it hangs");
        }
    }

    #[test]
    fn the_open_document_is_searched_as_it_stands_on_screen() {
        let root = folder("searchopen");
        std::fs::write(root.join("notebook.yaml"), "").unwrap();
        let file = root.join("a.kame");
        document(&file, "On disk.");
        let mut doc = Session::open(&file).unwrap().document().clone();
        doc.blocks[0].text = "On screen.".to_string();
        let found = notebook(&root);
        let mut all = Vec::new();
        let cancel = AtomicBool::new(false);
        search(
            &found,
            "screen",
            Some((file.as_path(), &doc)),
            &cancel,
            |hit| all.push(hit),
        );
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn a_long_line_is_cut_around_the_match_on_character_boundaries() {
        let root = folder("searchline");
        std::fs::write(root.join("notebook.yaml"), "").unwrap();
        let before = "é".repeat(200);
        let after = "—".repeat(200);
        document(&root.join("a.kame"), &format!("{before} needle {after}"));
        let all = hits(&notebook(&root), "needle");
        assert!(all[0].line.contains("needle"));
        assert!(all[0].line.chars().count() < 120, "{}", all[0].line);
    }

    #[test]
    fn a_search_that_is_cancelled_stops() {
        let root = folder("searchcancel");
        std::fs::write(root.join("notebook.yaml"), "").unwrap();
        document(&root.join("a.kame"), "word word word");
        let cancel = AtomicBool::new(true);
        let mut count = 0;
        search(&notebook(&root), "word", None, &cancel, |_| count += 1);
        assert_eq!(count, 0);
    }
}

mod linking {
    use super::*;
    use kame_notebook::links::{create_document, linking_to, remove_untouched, Follow};

    fn with_notebook(name: &str) -> (PathBuf, Notebook) {
        let root = folder(name);
        std::fs::write(root.join("notebook.yaml"), "").unwrap();
        let found = notebook(&root);
        (root, found)
    }

    #[test]
    fn a_missing_document_inside_the_notebook_can_be_created() {
        let (root, found) = with_notebook("create");
        let from = root.join("Notebook.kame");
        assert_eq!(
            found.follow("new", &from),
            Follow::Create(root.join("new.kame"))
        );
        assert_eq!(
            found.follow("a/b/new.kame", &from),
            Follow::Create(root.join("a/b/new.kame"))
        );
        assert_eq!(
            found.follow("a/new#Heading", &from),
            Follow::Create(root.join("a/new.kame")),
        );
        assert_eq!(
            found.follow("folder/", &from),
            Follow::Create(root.join("folder/Notebook.kame")),
            "a folder's document is its index"
        );
    }

    #[test]
    fn nothing_is_created_outside_the_notebook() {
        let (root, found) = with_notebook("outside");
        let from = root.join("a/doc.kame");
        std::fs::create_dir_all(root.join("a")).unwrap();
        for link in [
            "../../escape",
            "../../../tmp/escape.kame",
            "/tmp/escape.kame",
            "~/escape",
        ] {
            assert_eq!(found.follow(link, &from), Follow::Rotted, "{link}");
        }
        assert_eq!(
            found.follow("../inside", &from),
            Follow::Create(root.join("inside.kame"))
        );
        let outside = folder("elsewhere");
        std::os::unix::fs::symlink(&outside, root.join("away")).unwrap();
        assert_eq!(
            found.follow("../away/new", &from),
            Follow::Rotted,
            "through a link"
        );
    }

    #[test]
    fn only_what_could_be_a_document_is_created() {
        let (root, found) = with_notebook("kinds");
        let from = root.join("doc.kame");
        for link in [
            "static/paper.pdf",
            "paper.pdf",
            "theme/new",
            "static/new",
            ".hidden/new",
            "https://x.example/new",
        ] {
            assert_eq!(found.follow(link, &from), Follow::Rotted, "{link}");
        }
    }

    #[test]
    fn what_exists_opens_or_goes_to_the_system_viewer() {
        let (root, found) = with_notebook("exists");
        let from = root.join("Notebook.kame");
        document(&root.join("a.kame"), "A.");
        document(&root.join("folder/Notebook.kame"), "Index.");
        std::fs::write(root.join("page.md"), "Page.").unwrap();
        std::fs::create_dir_all(root.join("static")).unwrap();
        std::fs::write(root.join("static/paper.pdf"), "pdf").unwrap();
        std::fs::write(root.join("static/notes.md"), "notes").unwrap();
        assert_eq!(found.follow("a", &from), Follow::Open(root.join("a.kame")));
        assert_eq!(
            found.follow("a.kame#Part", &from),
            Follow::Open(root.join("a.kame"))
        );
        assert_eq!(
            found.follow("page", &from),
            Follow::Open(root.join("page.md"))
        );
        assert_eq!(
            found.follow("folder/", &from),
            Follow::Open(root.join("folder/Notebook.kame"))
        );
        assert_eq!(
            found.follow("folder", &from),
            Follow::Open(root.join("folder/Notebook.kame"))
        );
        assert_eq!(
            found.follow("static/paper.pdf", &from),
            Follow::View(root.join("static/paper.pdf"))
        );
        assert_eq!(
            found.follow("static/notes.md", &from),
            Follow::View(root.join("static/notes.md"))
        );
    }

    #[test]
    fn a_new_document_takes_the_heading_from_its_link() {
        let (root, _) = with_notebook("heading");
        let file = root.join("a/b/new.kame");
        create_document(&file, Some("The Intro")).unwrap();
        let session = Session::open(&file).unwrap();
        assert_eq!(session.document().blocks[0].text, "The Intro");
        assert!(session.heading_cursor("The Intro").is_some());
        assert_eq!(session.history(), (0, 0), "there's nothing to undo");
    }

    #[test]
    fn removing_an_untouched_document_takes_only_what_it_made() {
        let (root, _) = with_notebook("remove");
        std::fs::create_dir_all(root.join("kept")).unwrap();
        std::fs::write(root.join("kept/other.txt"), "x").unwrap();
        let file = root.join("kept/new/deeper/doc.kame");
        let made = create_document(&file, None).unwrap();
        drop(Session::open(&file).unwrap());
        remove_untouched(&file, &made);
        assert!(!file.exists());
        assert!(
            !root.join("kept/new").exists(),
            "the folders it made went too"
        );
        assert!(root.join("kept/other.txt").exists());
    }

    #[test]
    fn a_folder_someone_has_since_used_stays() {
        let (root, _) = with_notebook("removekeep");
        let file = root.join("made/doc.kame");
        let made = create_document(&file, None).unwrap();
        std::fs::write(root.join("made/other.txt"), "x").unwrap();
        remove_untouched(&file, &made);
        assert!(!file.exists());
        assert!(root.join("made/other.txt").exists());
    }

    #[test]
    fn what_links_here_finds_document_links_from_other_documents() {
        let (root, found) = with_notebook("backlinks");
        let target = root.join("target.kame");
        document(&target, "The target.");
        let link_in = |path: &Path, link: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut session = Session::open(path).unwrap();
            session.insert(&format!("See [[{link}]]"));
            session.apply_markdown_link();
            session.insert(" and on.");
        };
        link_in(&root.join("one.kame"), "target");
        link_in(&root.join("deep/two.kame"), "../target.kame#Part");
        link_in(&root.join("three.kame"), "other");
        document(&root.join("four.kame"), "Says target in words only.");
        link_in(&target, "target");
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let from: Vec<String> = linking_to(&found, &target, None, &cancel)
            .into_iter()
            .map(|link| link.relative)
            .collect();
        assert_eq!(from, ["deep/two.kame", "one.kame"], "and not itself");
    }
}
