//! A notebook on disk: made-up folders of documents.

use std::path::{Path, PathBuf};

use kame_core::session::Session;
use kame_notebook::list::{title_of, Kind};
use kame_notebook::Notebook;

pub fn folder(name: &str) -> PathBuf {
    kame_test_dir::folder(name).canonicalize().unwrap()
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
fn a_builds_output_is_no_part_of_the_notebook() {
    use kame_notebook::links::Follow;
    use kame_notebook::search::search;
    let root = folder("output");
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    document(&root.join("a.kame"), "Words.");
    std::fs::create_dir_all(root.join("output/deep")).unwrap();
    std::fs::write(root.join("output/index.html"), "built").unwrap();
    std::fs::write(root.join("output/deep/page.md"), "Words.").unwrap();
    document(
        &root.join("deep/output/kept.kame"),
        "A folder of that name elsewhere is fine.",
    );
    let found = notebook(&root);
    let listed: Vec<String> = found
        .entries()
        .into_iter()
        .map(|entry| entry.relative)
        .collect();
    assert_eq!(listed, ["a.kame", "deep/output/kept.kame"]);
    let mut hits = 0;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    search(&found, "Words", None, &cancel, |_| hits += 1);
    assert_eq!(hits, 1);
    let from = root.join("a.kame");
    assert_eq!(found.follow("output/new", &from), Follow::Rotted);
    assert_eq!(found.follow("output/", &from), Follow::Rotted);
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
        assert!(hits(&found, "e").len() > 3);
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
    fn a_relative_link_is_told_from_a_web_address_by_its_scheme_alone() {
        use kame_notebook::links::is_relative;
        for link in [
            "static/paper.pdf",
            "../a.kame",
            "papers/a:b.pdf",
            "./c:d",
            "~/x",
        ] {
            assert!(is_relative(link), "{link}");
        }
        for link in [
            "https://x.example/",
            "mailto:a@b.c",
            "//x.example/y",
            "#Heading",
            "a:b",
        ] {
            assert!(!is_relative(link), "{link}");
        }
    }

    #[test]
    fn only_named_schemes_are_web_links() {
        use kame_notebook::links::is_web_link;
        for link in [
            "https://x.example/",
            "HTTP://x.example/",
            "mailto:a@b.c",
            "//x.example/y",
            "ftp://x/",
            "tel:123",
        ] {
            assert!(is_web_link(link), "{link}");
        }
        for link in [
            "a:b",
            "Q3:plan.pdf",
            "zotero:abc",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "static/x.pdf",
            "#H",
            "",
        ] {
            assert!(!is_web_link(link), "{link}");
        }
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

mod lists {
    use super::*;
    use kame_notebook::list::{narrow, rank, rows, Entry, Kind, Listed};
    use std::collections::HashSet;

    fn listed(relative: &str, title: &str) -> Listed {
        Listed {
            entry: Entry {
                path: PathBuf::from("/nb").join(relative),
                relative: relative.to_string(),
                kind: Kind::Document,
            },
            title: title.to_string(),
        }
    }

    #[test]
    fn letters_need_only_appear_in_order_ignoring_case() {
        assert!(rank("mat", "Materialism", "x.kame").is_some());
        assert!(rank("MTR", "Materialism", "x.kame").is_some());
        assert!(rank("rtm", "Materialism", "x.kame").is_none());
        assert!(
            rank("deep", "Title", "deep/x.kame").is_some(),
            "the path counts"
        );
        assert!(rank("", "Anything", "x.kame").is_some());
    }

    #[test]
    fn closer_matches_come_first() {
        assert!(
            rank("mat", "Materialism", "x").unwrap()
                < rank("mat", "Maritain at home", "x").unwrap()
        );
        assert!(rank("mat", "Materialism", "x").unwrap() < rank("mat", "Dogmatics", "x").unwrap());
    }

    #[test]
    fn recent_documents_come_first_and_narrowing_leaves_the_rest_out() {
        let all = [
            listed("a.kame", "Alpha"),
            listed("b.kame", "Beta"),
            listed("c.kame", "Gamma"),
        ];
        let recent = [PathBuf::from("/nb/c.kame")];
        let order = |query: &str| -> Vec<&str> {
            narrow(query, &all, &recent)
                .into_iter()
                .map(|at| all[at].entry.relative.as_str())
                .collect()
        };
        assert_eq!(order(""), ["c.kame", "a.kame", "b.kame"]);
        assert_eq!(order("et"), ["b.kame"]);
        assert_eq!(
            order("a"),
            ["c.kame", "a.kame", "b.kame"],
            "ties go to the recent"
        );
        assert!(order("zzz").is_empty());
    }

    #[test]
    fn the_browser_shows_folders_and_hides_what_is_inside_closed_ones() {
        let entry = |relative: &str| Entry {
            path: PathBuf::from("/nb").join(relative),
            relative: relative.to_string(),
            kind: Kind::Document,
        };
        let entries = [
            entry("Notebook.kame"),
            entry("a/b/deep.kame"),
            entry("a/one.kame"),
            entry("z.kame"),
        ];
        let names = |open: &[&str]| -> Vec<(usize, String)> {
            let open: HashSet<String> = open.iter().map(|name| name.to_string()).collect();
            rows(&entries, &open)
                .into_iter()
                .map(|row| (row.depth, row.name))
                .collect()
        };
        let shut = names(&[]);
        assert_eq!(
            shut,
            [
                (0, "Notebook.kame".to_string()),
                (0, "a".to_string()),
                (0, "z.kame".to_string())
            ]
        );
        let open = names(&["a"]);
        assert_eq!(open[2], (1, "b".to_string()));
        assert_eq!(open[3], (1, "one.kame".to_string()));
        assert_eq!(names(&["a", "a/b"])[3], (2, "deep.kame".to_string()));
        assert_eq!(
            names(&["a/b"]).len(),
            3,
            "an open folder inside a closed one stays hidden"
        );
    }
}

#[test]
fn the_contents_page_counts_if_it_has_only_logs() {
    let root = folder("index");
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    assert_eq!(notebook(&root).index(), None);
    std::fs::create_dir_all(root.join(".Notebook.kame.sync")).unwrap();
    assert_eq!(notebook(&root).index(), Some(root.join("Notebook.kame")));
    std::fs::remove_dir_all(root.join(".Notebook.kame.sync")).unwrap();
    document(&root.join("Notebook.kame"), "Contents.");
    assert_eq!(notebook(&root).index(), Some(root.join("Notebook.kame")));
}

#[test]
fn a_list_is_read_again_only_when_what_is_there_has_changed() {
    use kame_notebook::list::{listed, listed_if_changed};
    let root = folder("relist");
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    document(&root.join("a.kame"), "A.");
    let found = notebook(&root);
    let known = listed(&found);
    assert_eq!(listed_if_changed(&found, &known), None);
    // As a new document arrives from another machine, with only its logs.
    std::fs::create_dir_all(root.join(".I like writing.kame.sync")).unwrap();
    let again = listed_if_changed(&found, &known).expect("something arrived");
    assert_eq!(again.len(), 2);
    assert_eq!(again[0].entry.relative, "I like writing.kame");
    assert_eq!(listed_if_changed(&found, &again), None);
    std::fs::remove_dir_all(root.join(".I like writing.kame.sync")).unwrap();
    assert_eq!(
        listed_if_changed(&found, &again).map(|list| list.len()),
        Some(1),
        "or left"
    );
}

mod starting {
    use super::*;
    use kame_notebook::init;

    #[test]
    fn a_new_notebook_has_settings_and_a_contents_page() {
        let parent = folder("init");
        let root = parent.join("deep/Philosophy");
        let made = init(&root).unwrap();
        assert_eq!(made.title(), "Philosophy");
        assert!(root.join("notebook.yaml").is_file());
        let index = made.index().unwrap();
        assert_eq!(index, made.root.join("Notebook.kame"));
        let session = Session::open(&index).unwrap();
        assert_eq!(session.document().blocks[0].text, "Philosophy");
        assert!(session.heading_cursor("Philosophy").is_some());
        assert_eq!(session.history(), (0, 0), "there's nothing to undo");
    }

    #[test]
    fn its_settings_file_reads_back_and_shows_what_can_go_in_it() {
        let root = folder("initread");
        init(&root).unwrap();
        let yaml = std::fs::read_to_string(root.join("notebook.yaml")).unwrap();
        for key in ["title", "url", "author", "remote"] {
            assert!(yaml.contains(key), "{yaml}");
        }
        assert_eq!(Notebook::open(&root).unwrap().settings, Default::default());
        let uncommented = yaml.replace("# title:", "title:");
        std::fs::write(root.join("notebook.yaml"), uncommented).unwrap();
        assert!(Notebook::open(&root).unwrap().settings.title.is_some());
    }

    #[test]
    fn a_notebook_can_begin_in_a_folder_that_already_has_documents() {
        let root = folder("initkeep");
        document(&root.join("Notebook.kame"), "My own contents.");
        document(&root.join("essay.kame"), "An essay.");
        init(&root).unwrap();
        let session = Session::open(&root.join("Notebook.kame")).unwrap();
        assert_eq!(
            session.document().blocks[0].text,
            "My own contents.",
            "kept"
        );
        assert_eq!(notebook(&root).documents().len(), 2);
    }

    #[test]
    fn a_folder_that_is_already_a_notebook_is_refused_and_left_alone() {
        let root = folder("initagain");
        std::fs::write(root.join("notebook.yaml"), "title: Mine\n").unwrap();
        let error = init(&root).unwrap_err();
        assert!(error.contains("already"), "{error}");
        assert_eq!(
            std::fs::read_to_string(root.join("notebook.yaml")).unwrap(),
            "title: Mine\n"
        );
        assert!(!root.join("Notebook.kame").exists());
    }

    #[test]
    fn a_file_in_the_way_is_refused() {
        let root = folder("initfile");
        std::fs::write(root.join("taken"), "x").unwrap();
        assert!(init(&root.join("taken")).is_err());
    }
}

/// A link that points nowhere yet would be written through, leaving the notebook.
#[test]
fn a_dangling_symbolic_link_cannot_lead_out() {
    use kame_notebook::links::Follow;
    let root = folder("dangling");
    let outside = folder("dangling-outside");
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    let from = root.join("a.kame");
    let link = |name: &str, target: &str| {
        std::os::unix::fs::symlink(outside.join(target), root.join(name)).unwrap();
    };
    let nb = notebook(&root);

    link("named.kame", "named.kame");
    assert_eq!(nb.follow("named.kame", &from), Follow::Rotted);

    // Named without its extension, which goes on after the first look.
    link("bare.kame", "bare.kame");
    assert_eq!(nb.follow("bare", &from), Follow::Rotted);

    // A folder's index.
    std::fs::create_dir(root.join("folder")).unwrap();
    std::os::unix::fs::symlink(
        outside.join("index.kame"),
        root.join("folder/Notebook.kame"),
    )
    .unwrap();
    assert_eq!(nb.follow("folder/", &from), Follow::Rotted);

    assert_eq!(
        std::fs::read_dir(&outside).unwrap().count(),
        0,
        "nothing was made outside"
    );

    // The same goes for links to something that's there.
    let there = outside.join("there.kame");
    document(&there, "Outside.");
    std::os::unix::fs::symlink(&there, root.join("here.kame")).unwrap();
    assert_eq!(nb.follow("here", &from), Follow::Rotted);
    std::fs::create_dir(root.join("other")).unwrap();
    std::os::unix::fs::symlink(&there, root.join("other/Notebook.kame")).unwrap();
    assert_eq!(nb.follow("other/", &from), Follow::Rotted);
}

/// Logs from a machine that wrote this document, with nothing here yet.
fn logs_for(document: &Path, words: &str) {
    let logs = kame_notebook::log_folder(document);
    let mut doc = kame_core::model::Document::new("t");
    doc.blocks[0].text = words.to_string();
    kame_sync::revlog::Replica {
        folder: logs,
        device: "elsewhere-0000".to_string(),
    }
    .sync(&doc, None, 1)
    .unwrap();
}

#[test]
fn a_folders_index_with_only_logs_is_there_already() {
    use kame_notebook::links::Follow;
    let root = folder("index-logs");
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    std::fs::create_dir(root.join("folder")).unwrap();
    let index = root.join("folder/Notebook.kame");
    logs_for(&index, "Contents.");
    let followed = notebook(&root).follow("folder/", &root.join("a.kame"));
    assert_eq!(followed, Follow::Open(index));
}

#[test]
fn a_document_named_markdown_is_read_as_markdown() {
    let root = folder("dot-markdown");
    std::fs::write(root.join("notebook.yaml"), "").unwrap();
    let file = root.join("old.markdown");
    std::fs::write(&file, "# A title\n\nThe words.\n").unwrap();
    let (doc, _) = kame_notebook::read_document(&file).unwrap();
    assert!(doc.blocks.iter().any(|block| block.text == "The words."));
}
