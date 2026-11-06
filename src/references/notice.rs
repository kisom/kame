//! The note about a key with no reference, as the caret closes it or an export finds it.

use kame_core::export::markdown_unresolved;
use kame_core::model::{Block, Document};
use kame_core::references::{block_citation_ranges, key_of, unresolved, References};

use crate::KameApp;

/// How the note about a key with no reference starts and ends, with the key between.
const NOTE_STARTS: &str = "No reference called ";
const NOTE_ENDS: &str = ". Ctrl-Shift-C adds it.";

/// The key a notice is the note about, if it is one.
pub(super) fn key_in_note(notice: &str) -> Option<&str> {
    notice.strip_prefix(NOTE_STARTS)?.strip_suffix(NOTE_ENDS)
}

/// What to say when the caret has just closed a key with no reference.
pub(crate) fn just_closed_note(
    block: &Block,
    caret: usize,
    references: &References,
) -> Option<String> {
    let closed = block_citation_ranges(block)
        .into_iter()
        .find(|(_, end)| *end == caret)?;
    let key = key_of(&block.text, closed);
    (!references.contains(key)).then(|| format!("{NOTE_STARTS}{key}{NOTE_ENDS}"))
}

/// What to add to an export's notice about keys that look lost. `marginals` is whether the format
/// carries them: HTML does, and Markdown, which doesn't, is also without footnotes that have no
/// text to hang on.
pub(crate) fn unresolved_note(
    doc: &Document,
    references: &References,
    marginals: bool,
) -> Option<String> {
    let lost = if marginals {
        unresolved(doc, references, true)
    } else {
        markdown_unresolved(doc, references)
    };
    (!lost.is_empty()).then(|| format!("no reference for {}", lost.join(", ")))
}

impl KameApp {
    /// Says so if typing `]` has just closed a key with no reference. A document outside a
    /// notebook has no references to add one to.
    pub(crate) fn note_closed_citation(&mut self) {
        if self.nb.notebook.is_none() {
            return;
        }
        let caret = self.session.valid_caret();
        let note = self
            .session
            .document()
            .blocks
            .iter()
            .find(|block| block.id == caret.block_id)
            .and_then(|block| just_closed_note(block, caret.offset, &self.refs.list));
        if let Some(note) = note {
            self.notice_tracker.announced();
            self.notice = Some(note);
        }
    }

    /// `unresolved_note` for the open document, if it's in a notebook.
    fn lost_keys_note(&self, marginals: bool) -> Option<String> {
        self.nb.notebook.as_ref()?;
        unresolved_note(self.session.document(), &self.refs.list, marginals)
    }

    /// What to say about an export, given what it did.
    pub(crate) fn export_notice(&self, done: String, marginals: bool) -> String {
        match self.lost_keys_note(marginals) {
            Some(note) => format!("{done}; {note}"),
            None => done,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::test_support::*;
    use crate::references::ReferencesUi;
    use crate::{test_temp_dir, Headless};
    use kame_core::model::{Anchor, Annotation, AnnotationKind, BlockKind};
    use kame_core::session::Session;

    #[test]
    fn closing_an_unknown_key_says_how_to_add_it() {
        let references = known(&["knuth1981"]);
        let note =
            |text: &str, caret: usize| just_closed_note(&paragraph(text), caret, &references);
        let closed = "as in [kleppmann2019]";

        assert_eq!(
            note(closed, closed.len()).as_deref(),
            Some("No reference called kleppmann2019. Ctrl-Shift-C adds it.")
        );
        // Having the shape of a key is enough, since the note doesn't interrupt.
        assert_eq!(
            note("[sic]", 5).as_deref(),
            Some("No reference called sic. Ctrl-Shift-C adds it.")
        );
        assert_eq!(note("as in [knuth1981]", 17), None, "a known key");
        assert_eq!(note("as in [a b]", 11), None, "not the shape of a key");
        assert_eq!(note(closed, 3), None, "a caret elsewhere");
        assert_eq!(note(closed, closed.len() - 1), None, "a caret inside it");

        let code = block(BlockKind::Code { language: None }, "[kleppmann2019]");
        assert_eq!(just_closed_note(&code, 15, &references), None);
    }

    #[test]
    fn an_export_with_a_lost_key_says_so() {
        let references = known(&["knuth1981"]);
        let mut doc = Document::new("t");
        doc.blocks = vec![paragraph("as [smith2020], [sic] and [knuth1981] say")];

        let note = unresolved_note(&doc, &references, true).expect("a lost key is named");
        assert!(note.contains("smith2020"), "{note}");
        assert!(!note.contains("sic"), "{note}");
        assert!(!note.contains("knuth1981"), "{note}");

        doc.blocks = vec![paragraph("as [sic] and [knuth1981] say")];
        assert_eq!(unresolved_note(&doc, &references, true), None);

        // A marginal counts only where the format carries it.
        doc.annotations.push(Annotation {
            id: uuid::Uuid::new_v4(),
            kind: AnnotationKind::Marginal,
            anchor: Anchor::block(doc.blocks[0].id),
            body: "[jones2019]".to_string(),
        });
        assert_eq!(unresolved_note(&doc, &references, false), None);
        let note = unresolved_note(&doc, &references, true).expect("a marginal's key is named");
        assert!(note.contains("jones2019"), "{note}");
    }

    #[test]
    fn a_markdown_notice_leaves_out_a_key_cited_only_in_a_dropped_footnote() {
        let references = known(&["knuth1981"]);
        let mut doc = Document::new("t");
        doc.blocks = vec![paragraph("as [knuth1981] says"), paragraph("")];
        doc.annotations.push(Annotation {
            id: uuid::Uuid::new_v4(),
            kind: AnnotationKind::Footnote,
            anchor: Anchor::range(doc.blocks[1].id, 0, 0),
            body: "see [smith2020]".to_string(),
        });
        assert_eq!(unresolved_note(&doc, &references, false), None);
        let note = unresolved_note(&doc, &references, true).expect("HTML lists the footnote");
        assert!(note.contains("smith2020"), "{note}");
    }

    #[test]
    fn typing_the_bracket_of_an_unknown_key_leaves_a_note_that_passes() {
        let mut page = page_in_notebook("");
        type_into(&mut page, "as in [kleppmann2019");
        assert_eq!(page.app.notice, None);

        type_into(&mut page, "]");
        assert_eq!(
            page.app.notice.as_deref(),
            Some("No reference called kleppmann2019. Ctrl-Shift-C adds it.")
        );

        // Typing joins up into words, so it's the next word that's the next edit.
        type_into(&mut page, " next");
        page.frame(Vec::new());
        assert_eq!(page.app.notice, None);
    }

    #[test]
    fn pasting_a_closed_unknown_key_leaves_no_note() {
        let mut page = page_in_notebook("");

        page.frame(vec![egui::Event::Paste(
            "as in [kleppmann2019]".to_string(),
        )]);
        page.frame(Vec::new());

        assert_eq!(
            page.app.session.document().blocks[0].text,
            "as in [kleppmann2019]"
        );
        assert_eq!(page.app.notice, None);
    }

    #[test]
    fn the_note_about_a_key_goes_when_the_key_is_added() {
        let mut page = noted_key_with_its_form();

        press_key(&mut page, egui::Key::Enter);
        page.frame(Vec::new());

        assert!(page.app.refs.list.contains("lamport1994"));
        assert_eq!(page.app.notice, None);
    }

    #[test]
    fn the_note_about_a_key_goes_when_its_file_arrives_from_elsewhere() {
        let mut page = page_in_notebook("");
        type_into(&mut page, "[lamport1994]");
        assert!(page.app.notice.is_some());

        write_reference(&root_of(&page), "lamport1994", GOOD);
        look_again(&mut page);

        assert_eq!(page.app.notice, None);
    }

    #[test]
    fn adding_a_key_leaves_a_notice_about_anything_else() {
        let mut page = noted_key_with_its_form();
        page.app.notice = Some("exported".to_string());

        press_key(&mut page, egui::Key::Enter);
        page.frame(Vec::new());

        assert!(page.app.refs.list.contains("lamport1994"));
        assert_eq!(page.app.notice.as_deref(), Some("exported"));
    }

    #[test]
    fn adding_a_key_leaves_the_note_about_another_key() {
        let mut page = page_in_notebook("");
        type_into(&mut page, "[lamport1994]");

        write_reference(&root_of(&page), "other2000", GOOD);
        look_again(&mut page);

        assert!(page.app.refs.list.contains("other2000"));
        assert_eq!(
            page.app.notice.as_deref(),
            Some("No reference called lamport1994. Ctrl-Shift-C adds it.")
        );
    }

    #[test]
    fn typing_the_bracket_of_a_known_key_says_nothing() {
        let mut page = page_in_notebook("");
        type_into(&mut page, "as in [knuth1981]");
        assert_eq!(page.app.notice, None);
    }

    #[test]
    fn the_same_note_twice_running_still_shows() {
        let mut page = page_in_notebook("[a2000 and [a2000");
        type_into(&mut page, "]");
        assert!(page.app.notice.is_some());

        // Moving the caret isn't an edit, so the note stays up until the next one, which says it
        // again.
        let block_id = page.app.session.document().blocks[0].id;
        page.app.session.set_caret(crate::edit::Cursor {
            block_id,
            offset: "[a2000".len(),
        });
        type_into(&mut page, "]");
        page.frame(Vec::new());

        assert_eq!(
            page.app.session.document().blocks[0].text,
            "[a2000] and [a2000]"
        );
        assert!(page.app.notice.is_some());
    }

    #[test]
    fn outside_a_notebook_no_note_is_left_for_a_key_or_an_export() {
        let folder = test_temp_dir().join(format!("outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let session = Session::open(&folder.join("doc.kame")).unwrap();
        let mut page = Headless::new(session);
        page.frame(Vec::new());

        type_into(&mut page, "[smith2020]");

        assert_eq!(page.app.notice, None);
        assert_eq!(
            page.app.export_notice("exported".to_string(), true),
            "exported"
        );
    }

    #[test]
    fn in_a_notebook_an_export_names_a_lost_key() {
        let mut page = page_in_notebook("");
        type_into(&mut page, "[smith2020] and [knuth1981]");

        let notice = page.app.export_notice("exported".to_string(), true);

        assert_eq!(notice, "exported; no reference for smith2020");
    }

    #[test]
    fn an_export_just_after_a_notebook_opens_has_its_references() {
        let root = notebook_folder("export-early");
        write_reference(&root, "knuth1981", GOOD);
        let mut session = Session::open(&root.join("doc.kame")).unwrap();
        session.insert("see [knuth1981]");
        let mut page = Headless::new(session);
        // The reference files are being read on another thread.
        page.frame(Vec::new());
        let (markdown, html) = (root.join("doc.md"), root.join("doc.html"));

        page.app.export_markdown_to(&markdown);
        page.app.refs = ReferencesUi::default();
        page.frame(Vec::new());
        page.app.export_html_to(&html);

        let markdown = std::fs::read_to_string(markdown).unwrap();
        assert!(markdown.contains("## References"), "{markdown}");
        assert!(
            markdown.contains("Breaking paragraphs into lines"),
            "{markdown}"
        );
        let html = std::fs::read_to_string(html).unwrap();
        assert!(html.contains("<h2>References</h2>"), "{html}");
    }
}
