//! A notebook's references, from the app: keeping up with its list, what a key looks like on the
//! page, the notes that come of keys with no reference, and the windows for citing and looking
//! after them. The page and the caret keys shape `with_citation_spans` of a block, a copy that is
//! never edited, so a known key is coloured, kept whole and clicked on as any link is. See
//! `docs/superpowers/specs/2026-11-04-references-design.md`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;
use egui::{Align2, Id, Order, Pos2, Vec2};
use kame_core::model::{Block, Document};
use kame_core::references::{
    block_citation_ranges, citation_at, cited, key_of, plain, suggest_key, unresolved, valid_key,
    with_citation_spans, Kind, Reference, References,
};
use kame_notebook::references::{citing, load, remove, rename, save, stamp, Renamed};
use kame_notebook::Notebook;

use super::notebook::{focus_field, list_keys, list_row, list_rows, list_window, BROWSER_INDENT};
use super::KameApp;

/// How long to go without looking for a change to the reference files, since the page asks every
/// frame.
const CHECK_EVERY: Duration = Duration::from_secs(1);
/// How far below a click its reference's window starts, to leave the line clicked on showing.
const BELOW_CLICK: f32 = 16.0;
/// How much of an entry a row of the search shows.
const SHOWN_OF_ENTRY: usize = 60;
/// How often to look for what a background thread has found.
const LOOK_AGAIN: Duration = Duration::from_millis(50);
/// What's wrong with a key that isn't one.
const NOT_A_KEY: &str = "A key is letters, digits, - and _, starting with a letter.";

/// The notebook's references, and which one is showing.
#[derive(Default)]
pub(crate) struct ReferencesUi {
    /// What the notebook on screen holds. Nothing outside one.
    pub(crate) list: References,
    /// What was wrong with the files left out of the list.
    problems: Vec<String>,
    /// The files the list was read from, as they were.
    stamp: Vec<(String, SystemTime, u64)>,
    /// The notebook the list was read from.
    root: Option<PathBuf>,
    /// When the files were last looked at.
    checked: Option<Instant>,
    /// The key whose entry is in its window.
    pub(crate) shown: Option<String>,
    /// Where the click that opened it was.
    shown_at: Pos2,
    /// The search Ctrl-Shift-C opens.
    cite_search: Option<CiteSearch>,
    /// The new reference or edit window.
    form: Option<FormWindow>,
    /// The References… window.
    window: Option<ListWindow>,
    /// How many documents cite each key, once counted.
    counts: Option<BTreeMap<String, usize>>,
    /// The count under way, on a background thread.
    counting: Option<Receiver<BTreeMap<String, Vec<PathBuf>>>>,
    /// Whether the list has changed since the count was started.
    recount: bool,
    /// Whether the last count stopped before finding anything.
    count_stopped: bool,
    /// The rename under way, on a background thread.
    renaming: Option<Renaming>,
}

/// The search over references: what's typed, and the row chosen.
struct CiteSearch {
    query: String,
    selected: usize,
    focus: bool,
}

/// The form in its window.
struct FormWindow {
    form: ReferenceForm,
    /// Why the last save didn't work.
    failed: Option<String>,
    /// Set for the frame a field should take the keyboard on.
    focus: bool,
    /// Whether a new reference is cited as it's added, or only added.
    cites: bool,
}

/// What the References… window is asking and has to say.
#[derive(Default)]
struct ListWindow {
    confirming: Option<Confirming>,
    /// Whether what's being asked has yet to be scrolled to, having been asked from elsewhere.
    reveal: bool,
    /// How the last delete or rename went.
    said: Vec<String>,
}

/// A confirmation row, under the key it's about.
enum Confirming {
    Delete {
        key: String,
    },
    Rename {
        key: String,
        new: String,
        focus: bool,
    },
}

impl Confirming {
    /// The key it's about.
    fn key(&self) -> &str {
        match self {
            Confirming::Delete { key } | Confirming::Rename { key, .. } => key,
        }
    }
}

/// A rename a background thread is doing, leaving the open document for when it's done.
struct Renaming {
    coming: Receiver<Result<Renamed, String>>,
    /// The notebook it's in, which is the one to remove the old file from.
    notebook: Notebook,
    /// The document it left alone, for this app to rewrite, and the old file with it.
    open: Option<PathBuf>,
    old: String,
    new: String,
}

/// The browser's row for the references, kept open or closed along with its folders. A folder's
/// name is relative to the notebook, so none can be this.
pub(crate) const BROWSER_SECTION: &str = "/references";

/// What choosing something among the browser's references asks for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BrowserReference {
    New,
    Edit(String),
    Rename(String),
    Delete(String),
}

/// What pressing something in the References… window asks for.
enum ListAction {
    Close,
    New,
    Edit(String),
    AskToDelete(String),
    AskToRename(String),
    Cancel,
    Delete(String),
    Rename(String, String),
}

/// What's known of how many documents cite a key.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Citing {
    Counting,
    /// The count stopped, so the windows go on without it.
    NotCounted,
    Documents(usize),
}

/// What Cite opens, going by the caret.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CiteStart {
    NeedsNotebook,
    New { key: String },
    Edit { key: String },
    Search,
}

/// What Cite opens for a caret at `offset` in `text`.
pub(crate) fn cite_start(
    text: &str,
    offset: usize,
    references: &References,
    in_notebook: bool,
) -> CiteStart {
    if !in_notebook {
        return CiteStart::NeedsNotebook;
    }
    let Some(range) = citation_at(text, offset) else {
        return CiteStart::Search;
    };
    let key = key_of(text, range).to_string();
    if references.contains(&key) {
        CiteStart::Edit { key }
    } else {
        CiteStart::New { key }
    }
}

/// The keys whose key, authors or title hold `query`, ignoring case, in `sorted()` order.
pub(crate) fn search<'a>(references: &'a References, query: &str) -> Vec<&'a str> {
    let query = query.trim().to_lowercase();
    let holds = |text: &str| text.to_lowercase().contains(&query);
    references
        .sorted()
        .into_iter()
        .filter(|(key, reference)| {
            holds(key)
                || holds(&reference.title)
                || reference.authors.iter().any(|name| holds(name))
        })
        .map(|(key, _)| key)
        .collect()
}

/// Where citing `key` puts it, for a caret at `offset`: nowhere if the caret is on it already, and
/// after a different key the caret is on rather than in the middle of it.
fn where_to_cite(text: &str, offset: usize, key: &str) -> Option<usize> {
    match citation_at(text, offset) {
        Some(range) if key_of(text, range) == key => None,
        Some((_, end)) => Some(end),
        None => Some(offset),
    }
}

/// A count of documents, in words.
fn documents(count: usize) -> String {
    match count {
        1 => "1 document".to_string(),
        _ => format!("{count} documents"),
    }
}

/// What a confirmation row says of how many documents cite a key.
fn cited_by(count: usize) -> String {
    match count {
        0 => "No document cites it.".to_string(),
        1 => "1 document cites it.".to_string(),
        _ => format!("{count} documents cite it."),
    }
}

/// What a confirmation row says before deleting `key`.
fn delete_warning(key: &str, citing: Citing) -> String {
    match citing {
        Citing::Counting => "Counting the documents that cite it…".to_string(),
        Citing::NotCounted => {
            format!("Its documents couldn't be counted. Any would then hold plain [{key}] text.")
        }
        Citing::Documents(0) => cited_by(0),
        Citing::Documents(1) => {
            format!("{} It would then hold plain [{key}] text.", cited_by(1))
        }
        Citing::Documents(count) => {
            format!(
                "{} They would then hold plain [{key}] text.",
                cited_by(count)
            )
        }
    }
}

/// What a confirmation row says before renaming a key.
fn rename_warning(citing: Citing) -> String {
    match citing {
        Citing::Counting => "Counting the documents that cite it…".to_string(),
        Citing::NotCounted => {
            "Its documents couldn't be counted. Any found are rewritten as an ordinary edit."
                .to_string()
        }
        Citing::Documents(0) => cited_by(0),
        Citing::Documents(1) => format!("{} It's rewritten as an ordinary edit.", cited_by(1)),
        Citing::Documents(count) => {
            format!("{} Each is rewritten as an ordinary edit.", cited_by(count))
        }
    }
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
    // Only a document with no file gets here, as `rename` had nothing open to leave the file for.
    if !renamed.old_kept && !renamed.failed.is_empty() {
        lines.push(format!(
            "{old} has gone, so [{old}] is plain text there until it's added again."
        ));
    }
    for (path, why) in &renamed.failed {
        let shown = path.strip_prefix(root).unwrap_or(path);
        lines.push(format!("{}: {why}", shown.display()));
    }
    lines
}

/// The form's two tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tab {
    #[default]
    Structured,
    Freeform,
}

/// What the new reference and edit windows hold while they're up. `reference` keeps everything
/// typed, so that switching kinds and tabs loses nothing, and `built` takes what will be saved.
pub(crate) struct ReferenceForm {
    pub(crate) tab: Tab,
    pub(crate) key: String,
    pub(crate) reference: Reference,
    /// The names as typed, as `names` reads them.
    pub(crate) authors_text: String,
    pub(crate) editors_text: String,
    /// Whether the key is the person's, and not to be suggested over.
    pub(crate) key_edited: bool,
    /// The key this is editing, if it isn't a new one.
    editing: Option<String>,
}

impl ReferenceForm {
    /// A form for a new reference, with `key` if there's one already.
    pub(crate) fn new(key: Option<String>) -> ReferenceForm {
        ReferenceForm {
            tab: Tab::default(),
            key_edited: key.is_some(),
            key: key.unwrap_or_default(),
            reference: Reference::default(),
            authors_text: String::new(),
            editors_text: String::new(),
            editing: None,
        }
    }

    /// A form for changing `reference`. Its key stays: renaming is in the References… window, as
    /// it rewrites documents.
    pub(crate) fn editing(key: &str, reference: &Reference) -> ReferenceForm {
        let freeform = reference.kind == Kind::Freeform;
        ReferenceForm {
            tab: if freeform {
                Tab::Freeform
            } else {
                Tab::Structured
            },
            key: key.to_string(),
            reference: Reference {
                // The kind the Structured tab starts on, should the person go there.
                kind: if freeform {
                    Kind::default()
                } else {
                    reference.kind
                },
                ..reference.clone()
            },
            authors_text: names_text(&reference.authors),
            editors_text: names_text(&reference.editors),
            key_edited: true,
            editing: Some(key.to_string()),
        }
    }

    fn is_new(&self) -> bool {
        self.editing.is_none()
    }

    /// The reference to save: only the fields its kind has, and freeform text exactly as typed.
    pub(crate) fn built(&self) -> Reference {
        let kind = match self.tab {
            Tab::Freeform => Kind::Freeform,
            Tab::Structured => self.reference.kind,
        };
        let mut typed = self.reference.clone();
        let mut built = Reference {
            kind,
            ..Reference::default()
        };
        for name in kind.fields() {
            match *name {
                "authors" => built.authors = names(&self.authors_text),
                "editors" => built.editors = names(&self.editors_text),
                "text" => built.text.clone_from(&typed.text),
                name => {
                    if let (Some(to), Some(from)) =
                        (text_field(&mut built, name), text_field(&mut typed, name))
                    {
                        *to = from.trim().to_string();
                    }
                }
            }
        }
        built
    }

    /// What's wrong with the key, if anything.
    pub(crate) fn key_problem(&self, references: &References) -> Option<String> {
        if self.key.is_empty() {
            return Some("It needs a key.".to_string());
        }
        if !valid_key(&self.key) {
            return Some(NOT_A_KEY.to_string());
        }
        if self.editing.as_deref() == Some(&self.key) {
            return None;
        }
        // Keys differing only in case can't both be saved, for the systems that ignore it.
        references
            .keys()
            .find(|key| key.eq_ignore_ascii_case(&self.key))
            .map(|key| format!("{key} is already a reference."))
    }

    /// What stops this being saved, if anything.
    fn problem(&self, references: &References) -> Option<String> {
        self.key_problem(references).or_else(|| {
            let built = self.built();
            if built.kind == Kind::Freeform && built.text.trim().is_empty() {
                return Some("text is missing".to_string());
            }
            built.check().err()
        })
    }

    pub(crate) fn can_save(&self, references: &References) -> bool {
        self.problem(references).is_none()
    }

    /// The entry as it would read.
    pub(crate) fn preview(&self) -> String {
        plain(&self.built())
    }

    /// Fills in a key from the fields, unless the key is the person's.
    pub(crate) fn suggest(&mut self, references: &References) {
        if self.key_edited {
            return;
        }
        let named = Reference {
            authors: names(&self.authors_text),
            ..self.reference.clone()
        };
        self.key = suggest_key(&named, references).unwrap_or_default();
    }
}

/// Names, each as spelled: split on semicolons if there are any, so that a name can hold a comma,
/// and otherwise on commas.
fn names(text: &str) -> Vec<String> {
    let separator = if text.contains(';') { ';' } else { ',' };
    text.split(separator)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// `names` as the form shows them, so that `names` reads them back the same. One name holding a
/// comma needs a semicolon even with nothing to separate.
fn names_text(names: &[String]) -> String {
    if !names.iter().any(|name| name.contains(',')) {
        return names.join(", ");
    }
    match names {
        [only] => format!("{only};"),
        names => names.join("; "),
    }
}

/// The field `name` of a reference, if it's one that holds a string.
fn text_field<'a>(reference: &'a mut Reference, name: &str) -> Option<&'a mut String> {
    Some(match name {
        "title" => &mut reference.title,
        "chapter" => &mut reference.chapter,
        "published_in" => &mut reference.published_in,
        "location" => &mut reference.location,
        "edition" => &mut reference.edition,
        "place" => &mut reference.place,
        "publisher" => &mut reference.publisher,
        "site" => &mut reference.site,
        "volume" => &mut reference.volume,
        "number" => &mut reference.number,
        "pages" => &mut reference.pages,
        "month" => &mut reference.month,
        "year" => &mut reference.year,
        "doi" => &mut reference.doi,
        "url" => &mut reference.url,
        "accessed" => &mut reference.accessed,
        "text" => &mut reference.text,
        _ => return None,
    })
}

/// What the form calls the field `name`.
fn field_label(name: &str) -> &'static str {
    match name {
        "authors" => "Authors",
        "editors" => "Editors",
        "title" => "Title",
        "chapter" => "Chapter",
        "published_in" => "Published in",
        "location" => "Location",
        "edition" => "Edition",
        "place" => "Place",
        "publisher" => "Publisher",
        "site" => "Site",
        "volume" => "Volume",
        "number" => "Number",
        "pages" => "Pages",
        "month" => "Month",
        "year" => "Year",
        "doi" => "DOI",
        "url" => "URL",
        "accessed" => "Accessed",
        _ => "",
    }
}

/// The freeform text box, which keeps Enter for itself.
fn freeform_text_id() -> Id {
    Id::unique("kame-reference-text")
}

fn enter_pressed(ctx: &egui::Context) -> bool {
    ctx.input(|input| {
        input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    repeat: false,
                    ..
                }
            )
        })
    })
}

/// The start of an entry, for a row with little room.
fn entry_start(reference: &Reference) -> String {
    let entry = plain(reference);
    match entry.char_indices().nth(SHOWN_OF_ENTRY) {
        Some((at, _)) => format!("{}…", &entry[..at]),
        None => entry,
    }
}

impl ReferencesUi {
    /// Keeps the list up with `notebook` and its files. Says what's wrong with them when that
    /// changes, as there's no use saying it every time they are looked at.
    pub(crate) fn refresh(&mut self, notebook: Option<&Notebook>) -> Option<String> {
        let Some(notebook) = notebook else {
            // A rename goes on, and its window stays until it has been read to say how it went.
            let renaming = self.renaming.take();
            let window = self
                .window
                .take()
                .filter(|window| renaming.is_some() || !window.said.is_empty());
            *self = ReferencesUi {
                renaming,
                window,
                ..ReferencesUi::default()
            };
            return None;
        };
        let moved = self.root.as_ref() != Some(&notebook.root);
        let due = self
            .checked
            .is_none_or(|checked| checked.elapsed() >= CHECK_EVERY);
        if !moved && !due {
            return None;
        }
        self.checked = Some(Instant::now());
        // Taken before reading, so that a file changing in between is read again next time.
        let files = stamp(notebook);
        if !moved && files == self.stamp {
            return None;
        }
        if moved {
            self.root = Some(notebook.root.clone());
            self.problems.clear();
            self.close_windows();
            self.counts = None;
            self.counting = None;
        }
        self.read(notebook, files)
    }

    /// Reads the list now, for a change made here that should show at once. The files may look no
    /// different from before, if it was quick and kept their length.
    fn reload(&mut self, notebook: &Notebook) -> Option<String> {
        self.checked = Some(Instant::now());
        self.read(notebook, stamp(notebook))
    }

    fn read(
        &mut self,
        notebook: &Notebook,
        files: Vec<(String, SystemTime, u64)>,
    ) -> Option<String> {
        self.stamp = files;
        self.recount = true;
        let loaded = load(notebook);
        self.list = loaded.references;
        let news = !loaded.problems.is_empty() && loaded.problems != self.problems;
        self.problems = loaded.problems;
        news.then(|| format!("this notebook's references: {}", self.problems.join("; ")))
    }

    /// Whether any of the windows for references is up.
    pub(crate) fn window_open(&self) -> bool {
        self.shown.is_some()
            || self.cite_search.is_some()
            || self.form.is_some()
            || self.window.is_some()
    }

    /// Closes everything, for another document or notebook, but for the window of a rename under
    /// way, which has yet to say how it went.
    pub(crate) fn close_windows(&mut self) {
        self.shown = None;
        self.cite_search = None;
        self.form = None;
        if self.renaming.is_none() {
            self.window = None;
        }
    }

    /// Closes the window on top, as Escape does. The form goes back to the window it came from,
    /// and a rename under way keeps its window up to say how it went.
    pub(crate) fn dismiss(&mut self) {
        if self.shown.take().is_some()
            || self.cite_search.take().is_some()
            || self.form.take().is_some()
            || self.renaming.is_some()
        {
            return;
        }
        let asking = self
            .window
            .as_mut()
            .is_some_and(|window| window.confirming.take().is_some());
        if !asking {
            self.window = None;
        }
    }

    fn open_form(&mut self, form: ReferenceForm) {
        self.form = Some(FormWindow {
            form,
            failed: None,
            focus: true,
            cites: true,
        });
    }

    /// Opens the form for a new reference that's only added, with nothing put in the document.
    fn open_form_to_add(&mut self) {
        self.open_form(ReferenceForm::new(None));
        if let Some(window) = &mut self.form {
            window.cites = false;
        }
    }

    /// Opens `key`'s entry, for a click at `at`.
    pub(crate) fn show(&mut self, key: &str, at: Pos2) {
        self.shown = Some(key.to_string());
        self.shown_at = at;
    }
}

/// `blocks` as the page draws them, with a link over each known key. They are only copied if there
/// is one, and the copy is for shaping and hit-testing, never editing.
pub(crate) fn page_blocks<'a>(blocks: &'a [Block], references: &References) -> Cow<'a, [Block]> {
    if references.is_empty() {
        return Cow::Borrowed(blocks);
    }
    let cited: Vec<Cow<Block>> = blocks
        .iter()
        .map(|block| with_citation_spans(block, references))
        .collect();
    if cited.iter().all(|block| matches!(block, Cow::Borrowed(_))) {
        return Cow::Borrowed(blocks);
    }
    Cow::Owned(cited.into_iter().map(Cow::into_owned).collect())
}

/// How the note about a key with no reference starts and ends, with the key between.
const NOTE_STARTS: &str = "No reference called ";
const NOTE_ENDS: &str = ". Ctrl-Shift-C adds it.";

/// The key a notice is the note about, if it is one.
fn key_in_note(notice: &str) -> Option<&str> {
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
/// carries them.
pub(crate) fn unresolved_note(
    doc: &Document,
    references: &References,
    marginals: bool,
) -> Option<String> {
    let lost = unresolved(doc, references, marginals);
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

    /// Draws the entry of the citation clicked on, until Escape or a press anywhere else.
    pub(crate) fn show_citation(&mut self, ctx: &egui::Context) {
        let Some(key) = self.refs.shown.as_deref() else {
            return;
        };
        let Some(reference) = self.refs.list.get(key) else {
            self.refs.shown = None;
            return;
        };
        let at = self.refs.shown_at;
        let mut edit = None;
        let drawn = egui::Window::new(key)
            .id(Id::unique("kame-citation"))
            .fixed_pos(at + Vec2::new(0.0, BELOW_CLICK))
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                ui.label(plain(reference));
                if ui.button("Edit").clicked() {
                    edit = Some(ReferenceForm::editing(key, reference));
                }
            });
        // A press at the same place is the click that opened this, in one frame on a quick tap.
        let pressed = ctx.input(|input| {
            input
                .pointer
                .interact_pos()
                .filter(|pos| input.pointer.any_pressed() && *pos != at)
        });
        let inside = |pos: Pos2| {
            drawn
                .as_ref()
                .is_some_and(|drawn| drawn.response.rect.contains(pos))
        };
        if pressed.is_some_and(|pos| !inside(pos)) {
            self.refs.shown = None;
        }
        if let Some(form) = edit {
            self.refs.shown = None;
            self.refs.open_form(form);
        }
    }

    /// The text of the block `caret` is in, to look for a key there. Nothing if the caret is on
    /// text that only looks like a key, in code or a link's words, as that is no key to open or
    /// to take for one already cited.
    fn text_for_citing(&self, caret: crate::edit::Cursor) -> &str {
        self.session
            .document()
            .blocks
            .iter()
            .find(|block| block.id == caret.block_id)
            .filter(|block| {
                citation_at(&block.text, caret.offset)
                    .is_none_or(|range| block_citation_ranges(block).contains(&range))
            })
            .map_or("", |block| block.text.as_str())
    }

    /// Cite: the chord, the menus and the right-click row. Does nothing over another dialog.
    pub(crate) fn begin_cite(&mut self) {
        if self.dialog_open() || self.close_flow.prompting {
            return;
        }
        let caret = self.session.valid_caret();
        let text = self.text_for_citing(caret);
        let in_notebook = self.nb.notebook.is_some();
        match cite_start(text, caret.offset, &self.refs.list, in_notebook) {
            CiteStart::NeedsNotebook => {
                self.notice_tracker.announced();
                self.notice = Some("Citing needs a notebook.".to_string());
            }
            CiteStart::New { key } => self.refs.open_form(ReferenceForm::new(Some(key))),
            CiteStart::Edit { key } => {
                if let Some(reference) = self.refs.list.get(&key) {
                    let form = ReferenceForm::editing(&key, reference);
                    self.refs.open_form(form);
                }
            }
            CiteStart::Search => {
                self.refs.cite_search = Some(CiteSearch {
                    query: String::new(),
                    selected: 0,
                    focus: true,
                });
            }
        }
    }

    /// Annotate > References… Over another dialog it says so, as a menu row that did nothing would
    /// look broken.
    pub(crate) fn open_references(&mut self) {
        if self.nb.notebook.is_none() || self.refs.window.is_some() {
            return;
        }
        if self.dialog_open() || self.close_flow.prompting {
            self.notice_tracker.announced();
            self.notice = Some("Close what's open, then choose References… again.".to_string());
            return;
        }
        self.refs.window = Some(ListWindow::default());
        self.refs.recount = true;
    }

    /// What was chosen among the browser's references. The browser can be clicked past a dialog,
    /// so this leaves one alone and says so, as what's typed in a form would be lost. Over the
    /// References… window it does what that window's own buttons do.
    pub(crate) fn browser_reference(&mut self, chosen: BrowserReference) {
        if self.other_dialog_open()
            || self.close_flow.prompting
            || self.refs.form.is_some()
            || self.refs.cite_search.is_some()
            || self.refs.renaming.is_some()
        {
            self.notice_tracker.announced();
            self.notice = Some("Close what's open, then choose that again.".to_string());
            return;
        }
        self.refs.shown = None;
        match chosen {
            BrowserReference::New => self.refs.open_form_to_add(),
            BrowserReference::Edit(key) => {
                if let Some(reference) = self.refs.list.get(&key) {
                    let form = ReferenceForm::editing(&key, reference);
                    self.refs.open_form(form);
                }
            }
            BrowserReference::Rename(key) => self.ask_in_references(Confirming::Rename {
                key,
                new: String::new(),
                focus: true,
            }),
            BrowserReference::Delete(key) => self.ask_in_references(Confirming::Delete { key }),
        }
    }

    /// Asks in the References… window, opening it if it isn't up.
    fn ask_in_references(&mut self, confirming: Confirming) {
        if self.refs.window.is_none() {
            self.refs.recount = true;
        }
        let window = self.refs.window.get_or_insert_with(ListWindow::default);
        window.said.clear();
        window.confirming = Some(confirming);
        window.reveal = true;
    }

    /// Reads the list after a change made here, so that it shows at once.
    fn reload_references(&mut self) {
        let Some(notebook) = self.nb.notebook.clone() else {
            return;
        };
        if let Some(problem) = self.refs.reload(&notebook) {
            self.session.last_error = Some(problem);
        }
    }

    /// Puts `[key]` in the document, as one ordinary edit, unless the caret is on it already. A
    /// selection is left in the document, with the key just after it, as citing is no way to
    /// replace words.
    fn cite(&mut self, key: &str) {
        if let Some((_, end)) = self.session.ordered_selection() {
            self.session.set_caret(end);
        }
        let caret = self.session.valid_caret();
        let Some(at) = where_to_cite(self.text_for_citing(caret), caret.offset, key) else {
            return;
        };
        if at != caret.offset {
            self.session.set_caret(crate::edit::Cursor {
                block_id: caret.block_id,
                offset: at,
            });
        }
        self.follow_caret = true;
        if !self.session.insert(&format!("[{key}]")) {
            let why = self.session.last_error.take();
            self.session.last_error = Some(format!(
                "could not cite {key} here — {}",
                why.as_deref().unwrap_or("this document can't be changed")
            ));
        }
    }

    /// Whether the keyboard is with something over the windows for references, so that an Enter
    /// is not for them: another dialog, the close prompt, or the find bar or a note's box if it
    /// has the keys. Those two can stay up beside a window without having them, and help takes
    /// no keys but F1 and Escape.
    fn keys_are_elsewhere(&self, ctx: &egui::Context) -> bool {
        self.other_dialog_open()
            || self.close_flow.prompting
            || ctx.memory(|memory| {
                memory.has_focus(super::find_query_id())
                    || memory.has_focus(super::note_body_edit_id())
            })
    }

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

    /// Takes in what the background threads have finished, and drops the note about a key once
    /// it has a reference, however it got one.
    pub(crate) fn references_tick(&mut self, ctx: &egui::Context) {
        let noted = self.notice.as_deref().and_then(key_in_note);
        if noted.is_some_and(|key| self.refs.list.contains(key)) {
            self.notice = None;
        }
        if let Some(coming) = &self.refs.counting {
            match coming.try_recv() {
                Ok(found) => {
                    self.refs.counts = Some(
                        found
                            .into_iter()
                            .map(|(key, documents)| (key, documents.len()))
                            .collect(),
                    );
                    self.refs.counting = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.refs.counting = None;
                    self.refs.count_stopped = true;
                }
                Err(TryRecvError::Empty) => ctx.request_repaint_after(LOOK_AGAIN),
            }
        }
        if let Some(renaming) = &self.refs.renaming {
            match renaming.coming.try_recv() {
                Ok(result) => self.finish_rename(result),
                Err(TryRecvError::Disconnected) => {
                    self.refs.renaming = None;
                    self.say_in_references(vec!["The rename stopped unexpectedly.".to_string()]);
                }
                Err(TryRecvError::Empty) => ctx.request_repaint_after(LOOK_AGAIN),
            }
        }
    }

    fn say_in_references(&mut self, said: Vec<String>) {
        if let Some(window) = &mut self.refs.window {
            window.said = said;
        }
    }

    fn start_counting(&mut self) {
        let Some(notebook) = self.nb.notebook.clone() else {
            return;
        };
        let (send, coming) = channel();
        self.refs.counts = None;
        self.refs.count_stopped = false;
        // Dropping an older one leaves it nowhere to send what it finds.
        self.refs.counting = Some(coming);
        std::thread::spawn(move || {
            let _ = send.send(citing(&notebook));
        });
    }

    fn start_rename(&mut self, old: String, new: String) {
        let Some(notebook) = self.nb.notebook.clone() else {
            return;
        };
        let open = self.session.path().map(Path::to_path_buf);
        let (send, coming) = channel();
        self.refs.renaming = Some(Renaming {
            coming,
            notebook: notebook.clone(),
            open: open.clone(),
            old: old.clone(),
            new: new.clone(),
        });
        std::thread::spawn(move || {
            let _ = send.send(rename(&notebook, &old, &new, open.as_deref()));
        });
    }

    /// The rename has done the other documents, and left the old file if a document is open. This
    /// does the open document, last because `rename` can refuse to start, and a refused rename
    /// should have touched nothing. The old file goes once everything has followed, so that a quit
    /// before now leaves `[old]` with its reference.
    fn finish_rename(&mut self, result: Result<Renamed, String>) {
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
        let old_is_left_here = open.is_some();
        // Left alone for being open, it's no longer what's open if it was saved as another file.
        if let Some(left) =
            open.filter(|open| !super::same_document(self.session.path(), Some(open)))
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
                .any(|(path, _)| super::same_document(source, Some(path)));
        if !source_failed {
            let here = self
                .session
                .path()
                .map_or_else(|| PathBuf::from("this document"), Path::to_path_buf);
            match self.rename_in_open_document(&old, &new) {
                Ok(true) => renamed.rewritten.push(here),
                Ok(false) => {}
                Err(why) => renamed.failed.push((here, why)),
            }
        }
        let mut not_removed = None;
        if old_is_left_here && renamed.failed.is_empty() {
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

    /// Renames `old` in the open document, returning whether it cited it, or why it still does.
    fn rename_in_open_document(&mut self, old: &str, new: &str) -> Result<bool, String> {
        let mut only_old = References::new();
        only_old.insert(old.to_string(), Reference::default());
        if cited(self.session.document(), &only_old, true).is_empty() {
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

    /// Draws whichever of the windows for citing and looking after references are up.
    pub(crate) fn show_reference_windows(&mut self, ctx: &egui::Context) {
        self.show_references_list(ctx);
        self.show_cite_search(ctx);
        self.show_reference_form(ctx);
    }

    fn show_cite_search(&mut self, ctx: &egui::Context) {
        let Some(mut search_list) = self.refs.cite_search.take() else {
            return;
        };
        let hits: Vec<String> = search(&self.refs.list, &search_list.query)
            .into_iter()
            .map(str::to_string)
            .collect();
        let rows_count = hits.len() + 1;
        // Keys taken here wouldn't reach what's on top of this. Escape needs nothing from here, as
        // `dismiss_dialog` has seen to it, knowing whether this is the one on top.
        let confirm = !self.keys_are_elsewhere(ctx)
            && list_keys(ctx, &mut search_list.selected, rows_count).1
            && !search_list.focus;
        let mut clicked = None;
        let references = &self.refs.list;
        let typed = search_list.query.trim().to_string();
        let new_row = if typed.is_empty() {
            "New reference…".to_string()
        } else {
            format!("New reference “{typed}”…")
        };
        list_window(ctx, "kame-cite", |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut search_list.query)
                    .hint_text("Cite a reference")
                    .return_key(None)
                    .desired_width(f32::INFINITY),
            );
            focus_field(&edit, &mut search_list.focus);
            if edit.changed() {
                search_list.selected = 0;
            }
            ui.separator();
            list_rows(ui, rows_count, search_list.selected, |ui, row, selected| {
                let (main, side) = match hits.get(row) {
                    Some(key) => (
                        key.as_str(),
                        references.get(key).map(entry_start).unwrap_or_default(),
                    ),
                    None => (new_row.as_str(), String::new()),
                };
                if list_row(ui, selected, main, &side).clicked() {
                    clicked = Some(row);
                }
            });
        });
        let chosen = clicked.or(confirm.then_some(search_list.selected));
        match chosen {
            Some(row) => match hits.get(row) {
                Some(key) => self.cite(key),
                None => {
                    let key = valid_key(&typed).then_some(typed);
                    self.refs.open_form(ReferenceForm::new(key));
                }
            },
            None => self.refs.cite_search = Some(search_list),
        }
    }

    /// The new reference and edit window.
    fn show_reference_form(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.refs.form.take() else {
            return;
        };
        let focus = std::mem::take(&mut window.focus);
        // A chord and an Enter arriving in the same frame would otherwise save a form nobody has
        // read, and in the text box Enter is a new line.
        let in_text_box = ctx.memory(|memory| memory.has_focus(freeform_text_id()));
        let mut submit =
            enter_pressed(ctx) && !focus && !in_text_box && !self.keys_are_elsewhere(ctx);
        let mut cancel = false;
        let is_new = window.form.is_new();
        let cites = window.cites;
        let title = if is_new {
            "New reference"
        } else {
            "Edit reference"
        };
        let references = &self.refs.list;
        let error_ink = self.palette.error_ink;
        egui::Window::new(title)
            .id(Id::unique("kame-reference-form"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                draw_form(ui, &mut window, focus, references);
                let problem = window.form.problem(references);
                if let Some(problem) = &problem {
                    ui.weak(problem);
                }
                if let Some(failed) = &window.failed {
                    ui.colored_label(error_ink, failed);
                }
                ui.horizontal(|ui| {
                    let save_button = match (is_new, cites) {
                        (true, true) => "Add and cite",
                        (true, false) => "Add",
                        (false, _) => "Save",
                    };
                    let button = egui::Button::new(save_button);
                    if ui.add_enabled(problem.is_none(), button).clicked() {
                        submit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            return;
        }
        if !submit || !window.form.can_save(&self.refs.list) {
            self.refs.form = Some(window);
            return;
        }
        let Some(notebook) = self.nb.notebook.clone() else {
            return;
        };
        let key = window.form.key.clone();
        if let Err(why) = save(&notebook, &key, &window.form.built()) {
            window.failed = Some(why);
            self.refs.form = Some(window);
            return;
        }
        self.reload_references();
        if is_new && cites {
            self.cite(&key);
        }
    }

    /// The References… window: every reference, with what can be done to it.
    fn show_references_list(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.refs.window.take() else {
            return;
        };
        if std::mem::take(&mut self.refs.recount) {
            self.start_counting();
        }
        let renaming = self.refs.renaming.is_some();
        let busy = renaming || self.refs.form.is_some();
        // The form on top of this has the keyboard.
        let enter = enter_pressed(ctx) && !busy && !self.keys_are_elsewhere(ctx);
        let in_notebook = self.refs.root.is_some();
        let count_stopped = self.refs.count_stopped;
        let error_ink = self.palette.error_ink;
        let (references, counts, problems) =
            (&self.refs.list, &self.refs.counts, &self.refs.problems);
        let mut action = None;
        egui::Window::new("References")
            .id(Id::unique("kame-references"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .show(ctx, |ui| {
                for problem in problems {
                    ui.colored_label(error_ink, problem);
                }
                if references.is_empty() && in_notebook {
                    ui.weak("No references yet.");
                }
                ui.add_enabled_ui(!busy, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (key, reference) in references.sorted() {
                                let citing = match counts {
                                    Some(counts) => Citing::Documents(
                                        counts.get(key).copied().unwrap_or_default(),
                                    ),
                                    None if count_stopped => Citing::NotCounted,
                                    None => Citing::Counting,
                                };
                                draw_list_row(ui, key, reference, citing, &mut action);
                                if let Some(confirming) = &mut window.confirming {
                                    draw_confirming(
                                        ui,
                                        key,
                                        confirming,
                                        citing,
                                        enter,
                                        &mut action,
                                    );
                                    // A window's first frame only measures it, and scrolls nothing.
                                    if window.reveal
                                        && confirming.key() == key
                                        && !ui.is_sizing_pass()
                                    {
                                        ui.scroll_to_cursor_animation(
                                            Some(egui::Align::BOTTOM),
                                            egui::style::ScrollAnimation::none(),
                                        );
                                        window.reveal = false;
                                    }
                                }
                                ui.separator();
                            }
                        });
                });
                if renaming {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Renaming…");
                    });
                }
                for line in &window.said {
                    ui.label(line);
                }
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!renaming, |ui| {
                        if ui.button("Close").clicked() {
                            action = Some(ListAction::Close);
                        }
                    });
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui.button("New reference…").clicked() {
                            action = Some(ListAction::New);
                        }
                    });
                });
            });
        match action {
            Some(ListAction::Close) => return,
            Some(ListAction::New) => {
                window.confirming = None;
                self.refs.open_form_to_add();
            }
            Some(ListAction::Edit(key)) => {
                window.confirming = None;
                if let Some(reference) = self.refs.list.get(&key) {
                    let form = ReferenceForm::editing(&key, reference);
                    self.refs.open_form(form);
                }
            }
            Some(ListAction::AskToDelete(key)) => {
                window.said.clear();
                window.confirming = Some(Confirming::Delete { key });
            }
            Some(ListAction::AskToRename(key)) => {
                window.said.clear();
                window.confirming = Some(Confirming::Rename {
                    new: String::new(),
                    key,
                    focus: true,
                });
            }
            Some(ListAction::Cancel) => window.confirming = None,
            Some(ListAction::Delete(key)) => {
                window.confirming = None;
                if let Some(notebook) = &self.nb.notebook {
                    window.said = vec![match remove(notebook, &key) {
                        Ok(()) => format!(
                            "Deleted {key}. Documents citing it now hold plain [{key}] text."
                        ),
                        Err(why) => why,
                    }];
                }
                self.reload_references();
            }
            Some(ListAction::Rename(old, new)) => {
                window.confirming = None;
                self.start_rename(old, new);
            }
            None => {}
        }
        self.refs.window = Some(window);
    }
}

/// The fields of the form, following the tab and the kind.
fn draw_form(ui: &mut egui::Ui, window: &mut FormWindow, focus: bool, references: &References) {
    let form = &mut window.form;
    ui.horizontal(|ui| {
        for (tab, label) in [(Tab::Structured, "Structured"), (Tab::Freeform, "Freeform")] {
            if ui.selectable_value(&mut form.tab, tab, label).changed() {
                window.focus = true;
            }
        }
    });
    if form.tab == Tab::Structured {
        ui.horizontal(|ui| {
            for (kind, label) in [
                (Kind::Article, "Article"),
                (Kind::Conference, "Conference"),
                (Kind::Book, "Book"),
                (Kind::Web, "Web"),
            ] {
                ui.selectable_value(&mut form.reference.kind, kind, label);
            }
        });
    }
    ui.separator();
    let mut changed = false;
    egui::Grid::new("kame-reference-fields")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Key");
            if form.is_new() {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut form.key)
                        .id_salt("kame-reference-key")
                        .return_key(None)
                        .desired_width(360.0)
                        .hint_text("suggested from the author and year"),
                );
                if field.changed() {
                    form.key_edited = true;
                }
                if focus && form.tab == Tab::Freeform && form.key.is_empty() {
                    field.request_focus();
                }
            } else {
                ui.label(&form.key)
                    .on_hover_text("Rename it from Annotate > References…");
            }
            ui.end_row();
            if form.tab == Tab::Structured {
                let kind = form.reference.kind;
                for (index, name) in kind.fields().iter().enumerate() {
                    ui.label(field_label(name));
                    let value = match *name {
                        "authors" => Some(&mut form.authors_text),
                        "editors" => Some(&mut form.editors_text),
                        name => text_field(&mut form.reference, name),
                    };
                    let Some(value) = value else {
                        ui.end_row();
                        continue;
                    };
                    let mut edit = egui::TextEdit::singleline(value)
                        .id_salt(*name)
                        .return_key(None)
                        .desired_width(360.0);
                    if matches!(*name, "authors" | "editors") {
                        edit = edit.hint_text("separated by commas, or ; if a name has a comma");
                    }
                    let field = ui.add(edit);
                    if focus && index == 0 {
                        field.request_focus();
                    }
                    changed |= field.changed();
                    ui.end_row();
                }
            }
        });
    if changed {
        form.suggest(references);
    }
    match form.tab {
        Tab::Structured => {
            ui.separator();
            ui.weak(form.preview());
        }
        Tab::Freeform => {
            let field = ui.add(
                egui::TextEdit::multiline(&mut form.reference.text)
                    .id(freeform_text_id())
                    .desired_rows(6)
                    .desired_width(440.0)
                    .hint_text("Goes into the References section exactly as typed"),
            );
            if focus && !(form.is_new() && form.key.is_empty()) {
                field.request_focus();
            }
        }
    }
}

/// The browser's section for the references, below its folders: a row that opens as a folder does,
/// with a row for each key and one for adding another. Returns whether that row was clicked, and
/// what was chosen from the rest.
pub(crate) fn draw_browser_references(
    ui: &mut egui::Ui,
    references: &References,
    open: bool,
) -> (bool, Option<BrowserReference>) {
    let mark = if open { "▾" } else { "▸" };
    let toggled = ui
        .selectable_label(false, format!("{mark} References"))
        .clicked();
    if !open {
        return (toggled, None);
    }
    let mut chosen = None;
    let indented = |ui: &mut egui::Ui, label: &str| {
        ui.horizontal(|ui| {
            ui.add_space(BROWSER_INDENT);
            ui.selectable_label(false, label)
        })
        .inner
    };
    for (key, _) in references.sorted() {
        let row = indented(ui, key);
        if row.clicked() {
            chosen = Some(BrowserReference::Edit(key.to_string()));
        }
        row.context_menu(|ui| {
            ui.set_min_width(120.0);
            if ui.button("Rename…").clicked() {
                chosen = Some(BrowserReference::Rename(key.to_string()));
                ui.close();
            } else if ui.button("Delete…").clicked() {
                chosen = Some(BrowserReference::Delete(key.to_string()));
                ui.close();
            }
        });
    }
    if indented(ui, "New reference…").clicked() {
        chosen = Some(BrowserReference::New);
    }
    (toggled, chosen)
}

/// A reference in the References… window.
fn draw_list_row(
    ui: &mut egui::Ui,
    key: &str,
    reference: &Reference,
    citing: Citing,
    action: &mut Option<ListAction>,
) {
    ui.horizontal(|ui| {
        ui.strong(key);
        ui.weak(match citing {
            Citing::Counting => "counting…".to_string(),
            Citing::NotCounted => "not counted".to_string(),
            Citing::Documents(count) => documents(count),
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Delete…").clicked() {
                *action = Some(ListAction::AskToDelete(key.to_string()));
            }
            if ui.small_button("Rename…").clicked() {
                *action = Some(ListAction::AskToRename(key.to_string()));
            }
            if ui.small_button("Edit").clicked() {
                *action = Some(ListAction::Edit(key.to_string()));
            }
        });
    });
    ui.weak(plain(reference));
}

/// The confirmation row, if it's about `key`. Nothing can be confirmed while the count it has to
/// say is coming.
fn draw_confirming(
    ui: &mut egui::Ui,
    key: &str,
    confirming: &mut Confirming,
    citing: Citing,
    enter: bool,
    action: &mut Option<ListAction>,
) {
    let counted = citing != Citing::Counting;
    match confirming {
        Confirming::Delete { key: asked } if asked == key => {
            ui.label(delete_warning(key, citing));
            ui.horizontal(|ui| {
                let delete = egui::Button::new(format!("Delete {key}"));
                if ui.add_enabled(counted, delete).clicked() {
                    *action = Some(ListAction::Delete(key.to_string()));
                }
                if ui.button("Cancel").clicked() {
                    *action = Some(ListAction::Cancel);
                }
            });
        }
        Confirming::Rename {
            key: asked,
            new,
            focus,
        } if asked == key => {
            ui.label(rename_warning(citing));
            let ready = counted && valid_key(new) && new != key;
            let opening = *focus;
            ui.horizontal(|ui| {
                ui.label("New key");
                let field = ui.add(
                    egui::TextEdit::singleline(new)
                        .hint_text(key)
                        .return_key(None)
                        .desired_width(200.0),
                );
                focus_field(&field, focus);
                let button = egui::Button::new(format!("Rename {key}"));
                let pressed = ui.add_enabled(ready, button).clicked();
                if pressed || (enter && ready && !opening) {
                    *action = Some(ListAction::Rename(key.to_string(), new.clone()));
                }
                if ui.button("Cancel").clicked() {
                    *action = Some(ListAction::Cancel);
                }
            });
            if new.is_empty() || new == key {
                ui.weak("Type the new key.");
            } else if !valid_key(new) {
                ui.weak(NOT_A_KEY);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_temp_dir, Headless};
    use kame_core::model::{Anchor, Annotation, AnnotationKind, BlockKind};
    use kame_core::session::Session;

    fn block(kind: BlockKind, text: &str) -> Block {
        Block {
            id: uuid::Uuid::new_v4(),
            kind,
            text: text.to_string(),
            spans: Vec::new(),
        }
    }

    fn paragraph(text: &str) -> Block {
        block(BlockKind::Paragraph, text)
    }

    fn known(keys: &[&str]) -> References {
        let mut references = References::new();
        for key in keys {
            references.insert(
                key.to_string(),
                Reference {
                    title: "Breaking paragraphs into lines".to_string(),
                    ..Reference::default()
                },
            );
        }
        references
    }

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
    fn the_page_gets_a_copy_with_links_over_known_keys_only() {
        let blocks = vec![
            paragraph("as [knuth1981] says"),
            paragraph("no [smith2020]"),
        ];

        let none = page_blocks(&blocks, &References::new());
        assert!(matches!(none, Cow::Borrowed(_)), "nothing to copy");

        let unrelated = page_blocks(&blocks, &known(&["other"]));
        assert!(
            matches!(unrelated, Cow::Borrowed(_)),
            "nothing cited is known"
        );

        let page = page_blocks(&blocks, &known(&["knuth1981"]));
        assert_eq!(page[0].spans.len(), 1);
        assert_eq!((page[0].spans[0].start, page[0].spans[0].end), (3, 14));
        assert_eq!(page[1], blocks[1]);
        assert_eq!(page[0].text, blocks[0].text);
        assert!(blocks[0].spans.is_empty(), "the document is left alone");
    }

    /// A notebook folder with an empty `references/` in it.
    fn notebook_folder(name: &str) -> std::path::PathBuf {
        let root = test_temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("references")).unwrap();
        std::fs::write(root.join("notebook.yaml"), "").unwrap();
        root
    }

    fn write_reference(root: &std::path::Path, key: &str, contents: &str) {
        std::fs::write(
            root.join("references").join(format!("{key}.yaml")),
            contents,
        )
        .unwrap();
    }

    const GOOD: &str = "kind: article\ntitle: Breaking paragraphs into lines\n";

    #[test]
    fn the_list_follows_the_files_and_the_notebook() {
        let first = notebook_folder("list-first");
        let second = notebook_folder("list-second");
        write_reference(&first, "knuth1981", GOOD);
        let first = Notebook::open(&first).unwrap();
        let second = Notebook::open(&second).unwrap();

        let mut refs = ReferencesUi::default();
        assert_eq!(refs.refresh(Some(&first)), None);
        assert!(refs.list.contains("knuth1981"));

        write_reference(&first.root, "smith2020", GOOD);
        refs.checked = None;
        assert_eq!(refs.refresh(Some(&first)), None);
        assert!(refs.list.contains("smith2020"), "a new file is seen");

        std::fs::remove_file(first.root.join("references/smith2020.yaml")).unwrap();
        // Not looked for yet, since it hasn't been long.
        refs.refresh(Some(&first));
        assert!(refs.list.contains("smith2020"));
        refs.checked = None;
        refs.refresh(Some(&first));
        assert!(!refs.list.contains("smith2020"), "a removed file is seen");

        refs.shown = Some("knuth1981".to_string());
        refs.refresh(Some(&second));
        assert!(refs.list.is_empty(), "another notebook, at once");
        assert_eq!(refs.shown, None);

        refs.refresh(Some(&first));
        assert!(refs.list.contains("knuth1981"));
        refs.refresh(None);
        assert!(refs.list.is_empty(), "outside a notebook there are none");
    }

    #[test]
    fn a_damaged_file_is_reported_once_and_the_others_still_load() {
        let root = notebook_folder("list-damaged");
        write_reference(&root, "knuth1981", GOOD);
        write_reference(&root, "broken2020", "title: [not closed\n");
        let notebook = Notebook::open(&root).unwrap();

        let mut refs = ReferencesUi::default();
        let problem = refs
            .refresh(Some(&notebook))
            .expect("the damage is reported");
        assert!(problem.contains("broken2020"), "{problem}");
        assert!(refs.list.contains("knuth1981"));

        write_reference(&root, "smith2020", GOOD);
        refs.checked = None;
        assert_eq!(refs.refresh(Some(&notebook)), None, "already said");
        assert!(refs.list.contains("smith2020"));
    }

    /// A page in a notebook holding `knuth1981`, with `text` as its only block.
    fn page_in_notebook(text: &str) -> Headless {
        let root = notebook_folder("page");
        write_reference(&root, "knuth1981", GOOD);
        let mut session = Session::open(&root.join("doc.kame")).unwrap();
        session.insert(text);
        let mut page = Headless::new(session);
        page.frame(Vec::new());
        page
    }

    /// Somewhere on the first line of the text, a little way in.
    fn near_the_start(page: &Headless) -> Pos2 {
        Pos2::new(page.body_left() + 12.0, crate::PAGE_PADDING + 8.0)
    }

    #[test]
    fn a_document_in_a_notebook_has_its_references() {
        let page = page_in_notebook("hello");
        assert!(page.app.refs.list.contains("knuth1981"));
    }

    #[test]
    fn a_document_outside_a_notebook_has_none() {
        let folder = test_temp_dir().join(format!("outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let session = Session::open(&folder.join("doc.kame")).unwrap();
        let mut page = Headless::new(session);
        page.frame(Vec::new());
        assert!(page.app.nb.notebook.is_none());
        assert!(page.app.refs.list.is_empty());
    }

    #[test]
    fn clicking_a_known_key_shows_it_and_leaves_the_caret() {
        let mut page = page_in_notebook("[knuth1981] says so");
        let caret = page.app.session.caret;

        page.click(near_the_start(&page));

        assert_eq!(page.app.refs.shown.as_deref(), Some("knuth1981"));
        assert_eq!(page.app.session.caret, caret);
        assert!(page.app.dialog_open());
        // The click that opened it isn't a click elsewhere, on this frame or the next.
        page.frame(Vec::new());
        assert_eq!(page.app.refs.shown.as_deref(), Some("knuth1981"));
    }

    #[test]
    fn clicking_an_unknown_key_places_the_caret() {
        let mut page = page_in_notebook("[smith2020] says so");
        let caret = page.app.session.caret;

        page.click(near_the_start(&page));

        assert_eq!(page.app.refs.shown, None);
        assert_ne!(page.app.session.caret, caret);
    }

    #[test]
    fn escape_closes_the_entry() {
        let mut page = page_in_notebook("[knuth1981] says so");
        page.click(near_the_start(&page));
        assert!(page.app.refs.shown.is_some());

        page.frame(vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);

        assert_eq!(page.app.refs.shown, None);
        assert!(!page.app.dialog_open());
    }

    #[test]
    fn clicking_elsewhere_closes_the_entry() {
        let mut page = page_in_notebook("[knuth1981] says so");
        page.click(near_the_start(&page));
        assert!(page.app.refs.shown.is_some());

        // Far enough right to land past the key, since a click below the text lands on its last
        // line.
        page.click(Pos2::new(page.body_left() + 300.0, 600.0));

        assert_eq!(page.app.refs.shown, None);
    }

    #[test]
    fn clicking_in_the_entry_keeps_it_open() {
        let mut page = page_in_notebook("[knuth1981] says so");
        page.click(near_the_start(&page));
        let caret = page.app.session.caret;
        page.frame(Vec::new());
        let inside = page.app.refs.shown_at + egui::Vec2::new(30.0, BELOW_CLICK + 30.0);

        page.click(inside);

        assert_eq!(page.app.refs.shown.as_deref(), Some("knuth1981"));
        assert_eq!(
            page.app.session.caret, caret,
            "the page didn't get the click"
        );
    }

    #[test]
    fn a_quick_tap_in_one_frame_opens_the_entry_and_leaves_it_open() {
        let mut page = page_in_notebook("[knuth1981] says so");
        let pos = near_the_start(&page);
        page.frame(vec![egui::Event::PointerMoved(pos)]);

        page.frame(
            [true, false]
                .map(|pressed| egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                })
                .to_vec(),
        );

        assert_eq!(page.app.refs.shown.as_deref(), Some("knuth1981"));
    }

    #[test]
    fn a_double_click_selects_the_key_and_closes_the_entry() {
        let mut page = page_in_notebook("[knuth1981] says so");

        page.double_click(near_the_start(&page));

        assert_eq!(page.app.refs.shown, None);
        assert!(page.app.session.selection.is_some());
    }

    #[test]
    fn another_document_in_the_notebook_closes_the_entry() {
        let mut page = page_in_notebook("[knuth1981] says so");
        page.click(near_the_start(&page));
        assert!(page.app.refs.shown.is_some());
        let root = page.app.nb.notebook.clone().unwrap().root;

        let other = Session::open(&root.join("other.kame")).unwrap();
        page.app.adopt_session(other);
        page.frame(Vec::new());

        assert_eq!(page.app.refs.shown, None);
        assert!(!page.app.dialog_open());
        assert_eq!(
            page.app.nb.notebook.as_ref().map(|nb| &nb.root),
            Some(&root)
        );
        assert!(page.app.refs.list.contains("knuth1981"), "the same list");
        // The keyboard is the new document's.
        type_into(&mut page, "x");
        assert_eq!(page.app.session.document().blocks[0].text, "x");
    }

    #[test]
    fn an_entry_whose_reference_is_gone_closes() {
        let mut page = page_in_notebook("[knuth1981] says so");
        page.click(near_the_start(&page));
        page.app.refs.list = References::new();

        page.frame(Vec::new());

        assert_eq!(page.app.refs.shown, None);
    }

    /// Types `text` a character at a time, as the keyboard does.
    fn type_into(page: &mut Headless, text: &str) {
        for character in text.chars() {
            page.frame(vec![egui::Event::Text(character.to_string())]);
        }
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

    /// `[lamport1994]` typed so that its note is up, and the form for it open with a title.
    fn noted_key_with_its_form() -> Headless {
        let mut page = page_in_notebook("");
        type_into(&mut page, "[lamport1994]");
        assert!(page.app.notice.is_some());
        cite_chord(&mut page);
        let window = page.app.refs.form.as_mut().expect("the form opens");
        window.form.reference.title = "How to write".to_string();
        page
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
        page.app.refs.checked = None;
        page.frame(Vec::new());

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
        page.app.refs.checked = None;
        page.frame(Vec::new());

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
    fn cite_opens_what_the_caret_is_on() {
        let references = known(&["knuth1981"]);
        let start = |text: &str, offset: usize| cite_start(text, offset, &references, true);
        let new = CiteStart::New {
            key: "new1".to_string(),
        };
        let edit = CiteStart::Edit {
            key: "knuth1981".to_string(),
        };

        assert_eq!(
            cite_start("[knuth1981]", 3, &references, false),
            CiteStart::NeedsNotebook
        );
        assert_eq!(start("see [new1] here", 6), new, "in an unknown key");
        assert_eq!(start("see [new1] here", 10), new, "just after it");
        assert_eq!(start("see [knuth1981] here", 6), edit, "in a known key");
        assert_eq!(start("see [knuth1981] here", 15), edit, "just after it");
        assert_eq!(
            start("see [knuth1981] here", 4),
            CiteStart::Search,
            "before it"
        );
        assert_eq!(
            start("see [knuth1981] here", 16),
            CiteStart::Search,
            "past it"
        );
        assert_eq!(start("plain words", 3), CiteStart::Search);
        assert_eq!(
            start("in [a b] here", 6),
            CiteStart::Search,
            "not a key's shape"
        );
        assert_eq!(start("", 0), CiteStart::Search);
        // Offsets are bytes, so a multibyte letter before the key has to be counted as such.
        assert_eq!(start("é [new1]", "é [new1]".len()), new);
    }

    #[test]
    fn search_looks_through_keys_authors_and_titles() {
        let mut references = References::new();
        references.insert(
            "zeta".to_string(),
            Reference {
                authors: vec!["D. E. Knuth".to_string()],
                title: "Breaking paragraphs".to_string(),
                ..Reference::default()
            },
        );
        references.insert(
            "Alpha".to_string(),
            Reference {
                title: "Knuth and his lines".to_string(),
                ..Reference::default()
            },
        );
        references.insert(
            "knuth1981".to_string(),
            Reference {
                title: "Élan".to_string(),
                ..Reference::default()
            },
        );

        assert_eq!(search(&references, "KNUTH"), ["Alpha", "knuth1981", "zeta"]);
        assert_eq!(search(&references, "breaking"), ["zeta"]);
        assert_eq!(search(&references, "alpha"), ["Alpha"]);
        assert_eq!(search(&references, "élan"), ["knuth1981"]);
        assert_eq!(search(&references, "  zeta "), ["zeta"]);
        assert_eq!(search(&references, "nothing"), Vec::<&str>::new());
        assert_eq!(
            search(&references, ""),
            ["Alpha", "knuth1981", "zeta"],
            "everything, with nothing typed"
        );
    }

    #[test]
    fn names_split_on_commas_and_keep_their_spelling() {
        let mut form = ReferenceForm::new(Some("k".to_string()));
        form.reference.kind = Kind::Book;
        form.reference.title = "T".to_string();
        form.authors_text = "D. E. Knuth,  M. F. Plass ,, van der Berg".to_string();
        form.editors_text = "Ünal Çelik".to_string();

        let built = form.built();

        assert_eq!(
            built.authors,
            ["D. E. Knuth", "M. F. Plass", "van der Berg"]
        );
        assert_eq!(built.editors, ["Ünal Çelik"]);
        // And back again, as an edit shows them.
        let again = ReferenceForm::editing("k", &built);
        assert_eq!(again.authors_text, "D. E. Knuth, M. F. Plass, van der Berg");
        assert_eq!(again.editors_text, "Ünal Çelik");
        assert_eq!(again.built(), built);
    }

    #[test]
    fn a_name_holding_a_comma_comes_back_whole_from_the_form() {
        let saved = |authors: &[&str], editors: &[&str]| Reference {
            kind: Kind::Book,
            title: "T".to_string(),
            authors: authors.iter().map(|name| name.to_string()).collect(),
            editors: editors.iter().map(|name| name.to_string()).collect(),
            ..Reference::default()
        };

        let two = saved(&["M. L. King, Jr.", "J. Doe"], &["Doe, J."]);
        let form = ReferenceForm::editing("k", &two);
        assert_eq!(form.authors_text, "M. L. King, Jr.; J. Doe");
        assert_eq!(form.built(), two);

        // On its own a name still needs a semicolon, or its comma would split it.
        let one = saved(&["M. L. King, Jr."], &[]);
        let form = ReferenceForm::editing("k", &one);
        assert_eq!(form.authors_text, "M. L. King, Jr.;");
        assert_eq!(form.built(), one);

        // Names with no comma look as they always did.
        let plain_names = saved(&["D. E. Knuth", "M. F. Plass"], &[]);
        let form = ReferenceForm::editing("k", &plain_names);
        assert_eq!(form.authors_text, "D. E. Knuth, M. F. Plass");
        assert_eq!(form.built(), plain_names);
    }

    #[test]
    fn a_semicolon_in_the_names_is_what_splits_them() {
        assert_eq!(names("King, M. L.; Doe, J."), ["King, M. L.", "Doe, J."]);
        assert_eq!(names("King, Jr.;"), ["King, Jr."]);
        assert_eq!(names(" ; ; A ;; "), ["A"]);
        assert_eq!(names("A, B"), ["A", "B"]);
    }

    #[test]
    fn a_suggested_key_follows_the_fields_until_its_edited() {
        let references = known(&["knuth1981"]);
        let mut form = ReferenceForm::new(None);
        assert!(!form.key_edited);
        assert_eq!(form.key, "");

        form.authors_text = "L. Lamport".to_string();
        form.reference.year = "1994".to_string();
        form.suggest(&references);
        assert_eq!(form.key, "lamport1994");

        form.authors_text = "D. E. Knuth, M. F. Plass".to_string();
        form.reference.year = "1981".to_string();
        form.suggest(&references);
        assert_eq!(form.key, "knuth1981a", "the plain one is taken");

        // With no letter left of the name, the person types one.
        form.authors_text = "李".to_string();
        form.suggest(&references);
        assert_eq!(form.key, "");

        form.key = "mine".to_string();
        form.key_edited = true;
        form.authors_text = "L. Lamport".to_string();
        form.suggest(&references);
        assert_eq!(form.key, "mine");

        // A key that came with the form was already chosen.
        let mut given = ReferenceForm::new(Some("given".to_string()));
        given.authors_text = "L. Lamport".to_string();
        given.suggest(&references);
        assert_eq!(given.key, "given");
    }

    #[test]
    fn the_form_refuses_a_bad_key_a_taken_key_and_no_title() {
        let references = known(&["knuth1981"]);
        let mut form = ReferenceForm::new(Some("smith2020".to_string()));
        form.reference.title = "A title".to_string();
        assert_eq!(form.key_problem(&references), None);
        assert!(form.can_save(&references));

        for bad in ["", "1bad", "no way", "é"] {
            form.key = bad.to_string();
            assert!(form.key_problem(&references).is_some(), "{bad:?}");
            assert!(!form.can_save(&references), "{bad:?}");
        }
        for taken in ["knuth1981", "Knuth1981"] {
            form.key = taken.to_string();
            let problem = form.key_problem(&references).expect("a taken key");
            assert!(problem.contains("knuth1981"), "{problem}");
            assert!(!form.can_save(&references), "{taken:?}");
        }

        form.key = "smith2020".to_string();
        for missing in ["", "   "] {
            form.reference.title = missing.to_string();
            assert_eq!(form.key_problem(&references), None);
            assert!(!form.can_save(&references), "{missing:?}");
        }

        // A taken key is allowed when it's the key being edited.
        let editing = ReferenceForm::editing("knuth1981", references.get("knuth1981").unwrap());
        assert_eq!(editing.key_problem(&references), None);
        assert!(editing.can_save(&references));
    }

    #[test]
    fn freeform_saves_only_a_key_and_text_and_shows_them_as_typed() {
        let typed = "A letter,  1 May 1984.\nSecond line: \"quoted\" ";
        let mut form = ReferenceForm::new(Some("letter".to_string()));
        form.authors_text = "D. E. Knuth".to_string();
        form.reference.title = "Left on the other tab".to_string();
        form.reference.text = typed.to_string();
        form.tab = Tab::Freeform;

        assert!(form.can_save(&References::new()));
        assert_eq!(
            form.built(),
            Reference {
                kind: Kind::Freeform,
                text: typed.to_string(),
                ..Reference::default()
            }
        );
        assert_eq!(form.preview(), typed);

        // What was typed on the other tab is still there.
        form.tab = Tab::Structured;
        assert_eq!(form.built().title, "Left on the other tab");
        assert_eq!(form.built().text, "");

        // Text that's only spaces is no text.
        form.tab = Tab::Freeform;
        form.reference.text = " \n".to_string();
        assert!(!form.can_save(&References::new()));

        // A freeform reference opens on its tab, and keeps its text.
        let saved = Reference {
            kind: Kind::Freeform,
            text: typed.to_string(),
            ..Reference::default()
        };
        let editing = ReferenceForm::editing("letter", &saved);
        assert_eq!(editing.tab, Tab::Freeform);
        assert_eq!(editing.built(), saved);
    }

    #[test]
    fn what_the_form_builds_always_passes_check() {
        for kind in [
            Kind::Article,
            Kind::Conference,
            Kind::Book,
            Kind::Web,
            Kind::Freeform,
        ] {
            for tab in [Tab::Structured, Tab::Freeform] {
                let mut form = ReferenceForm::new(Some("k".to_string()));
                form.tab = tab;
                form.reference = Reference {
                    kind,
                    title: "T".to_string(),
                    chapter: "1".to_string(),
                    published_in: "J".to_string(),
                    location: "L".to_string(),
                    edition: "2".to_string(),
                    place: "P".to_string(),
                    publisher: "Q".to_string(),
                    site: "S".to_string(),
                    volume: "3".to_string(),
                    number: "4".to_string(),
                    pages: "5".to_string(),
                    month: "May".to_string(),
                    year: "1981".to_string(),
                    doi: "10.1/x".to_string(),
                    url: "https://example.com".to_string(),
                    accessed: "1 May 2020".to_string(),
                    text: "Words".to_string(),
                    ..Reference::default()
                };
                form.authors_text = "A. Author".to_string();
                form.editors_text = "E. Editor".to_string();
                assert_eq!(form.built().check(), Ok(()), "{kind:?} on {tab:?}");
                assert!(form.can_save(&References::new()), "{kind:?} on {tab:?}");
            }
        }
    }

    #[test]
    fn a_citation_goes_at_the_caret_unless_the_caret_is_on_it() {
        assert_eq!(where_to_cite("plain words", 5, "k"), Some(5));
        assert_eq!(where_to_cite("see [k] here", 6, "k"), None, "in it");
        assert_eq!(where_to_cite("see [k] here", 7, "k"), None, "just after it");
        assert_eq!(where_to_cite("see [k] here", 4, "k"), Some(4), "before it");
        // A key other than the one being cited isn't broken in two.
        assert_eq!(where_to_cite("see [other] here", 6, "k"), Some(11));
        assert_eq!(where_to_cite("see [other] here", 11, "k"), Some(11));
    }

    #[test]
    fn how_many_documents_cite_a_key_is_said_in_words() {
        assert_eq!(cited_by(0), "No document cites it.");
        assert_eq!(cited_by(1), "1 document cites it.");
        assert_eq!(cited_by(3), "3 documents cite it.");
    }

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

    /// A key pressed and let go with `modifiers`, as egui takes a key still down for a repeat.
    fn key(page: &mut Headless, key: egui::Key, modifiers: egui::Modifiers) {
        page.frame(
            [true, false]
                .map(|pressed| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers,
                })
                .to_vec(),
        );
    }

    fn press_key(page: &mut Headless, pressed: egui::Key) {
        key(page, pressed, egui::Modifiers::NONE);
    }

    /// Ctrl-Shift-C, and the frame after it for a window to be drawn and take the keyboard.
    fn cite_chord(page: &mut Headless) {
        let modifiers = egui::Modifiers {
            command: true,
            shift: true,
            ..egui::Modifiers::NONE
        };
        key(page, egui::Key::C, modifiers);
        page.frame(Vec::new());
    }

    /// Runs frames until `done`, for what a background thread is doing.
    fn run_until(page: &mut Headless, done: impl Fn(&KameApp) -> bool) {
        for _ in 0..2000 {
            page.frame(Vec::new());
            if done(&page.app) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("it never finished");
    }

    /// Opens the References… window and waits for its counts.
    fn open_list(page: &mut Headless) {
        page.app.open_references();
        run_until(page, |app| app.refs.counts.is_some());
        page.frame(Vec::new());
    }

    fn only_text(page: &Headless) -> &str {
        &page.app.session.document().blocks[0].text
    }

    fn root_of(page: &Headless) -> PathBuf {
        page.app.nb.notebook.as_ref().unwrap().root.clone()
    }

    fn reference_file(page: &Headless, key: &str) -> PathBuf {
        root_of(page).join("references").join(format!("{key}.yaml"))
    }

    /// A document beside the open one, saved with `text` in it.
    fn other_document(page: &Headless, name: &str, text: &str) -> PathBuf {
        let path = root_of(page).join(name);
        let mut session = Session::open(&path).unwrap();
        session.insert(text);
        path
    }

    fn text_on_disk(path: &Path) -> String {
        kame_notebook::read_document(path).unwrap().0.blocks[0]
            .text
            .clone()
    }

    #[test]
    fn citing_outside_a_notebook_says_it_needs_one() {
        let folder = test_temp_dir().join(format!("outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let mut page = Headless::new(Session::open(&folder.join("doc.kame")).unwrap());
        page.frame(Vec::new());

        cite_chord(&mut page);

        assert_eq!(page.app.notice.as_deref(), Some("Citing needs a notebook."));
        assert!(!page.app.dialog_open());
    }

    #[test]
    fn choosing_from_the_search_inserts_the_key_as_one_undo() {
        let mut page = page_in_notebook("as said by ");
        cite_chord(&mut page);
        assert!(page.app.dialog_open());

        // What's typed goes to the search, not the document.
        type_into(&mut page, "KNUTH");
        assert_eq!(only_text(&page), "as said by ");

        press_key(&mut page, egui::Key::Enter);

        assert_eq!(only_text(&page), "as said by [knuth1981]");
        assert!(!page.app.dialog_open());
        assert_eq!(page.app.session.caret.offset, only_text(&page).len());
        assert!(page.app.session.undo());
        assert_eq!(only_text(&page), "as said by ", "only the key goes");
    }

    #[test]
    fn up_and_down_choose_a_row_of_the_search() {
        let mut page = page_in_notebook("");
        write_reference(&root_of(&page), "smith2020", GOOD);
        page.app.reload_references();
        cite_chord(&mut page);

        press_key(&mut page, egui::Key::ArrowDown);
        press_key(&mut page, egui::Key::ArrowDown);
        press_key(&mut page, egui::Key::ArrowUp);
        press_key(&mut page, egui::Key::Enter);

        assert_eq!(only_text(&page), "[smith2020]");
    }

    #[test]
    fn clicking_a_row_of_the_search_chooses_it() {
        let mut page = page_in_notebook("");
        cite_chord(&mut page);

        page.press("knuth1981");

        assert_eq!(only_text(&page), "[knuth1981]");
    }

    #[test]
    fn the_searchs_last_row_is_a_new_reference_with_the_query_as_its_key() {
        let mut page = page_in_notebook("");
        cite_chord(&mut page);
        type_into(&mut page, "lamport1994");
        page.frame(Vec::new());
        assert!(page.sees("New reference “lamport1994”…"));

        press_key(&mut page, egui::Key::Enter);

        let window = page.app.refs.form.as_ref().expect("the form opens");
        assert_eq!(window.form.key, "lamport1994");
        assert!(page.app.refs.cite_search.is_none());
        assert_eq!(only_text(&page), "");
    }

    #[test]
    fn a_query_that_cant_be_a_key_leaves_the_key_to_be_suggested() {
        let mut page = page_in_notebook("");
        cite_chord(&mut page);
        type_into(&mut page, "two words");

        press_key(&mut page, egui::Key::Enter);

        let form = &page.app.refs.form.as_ref().expect("the form opens").form;
        assert_eq!(form.key, "");
        assert!(!form.key_edited);
    }

    #[test]
    fn add_and_cite_saves_inserts_and_shows_at_once() {
        let mut page = page_in_notebook("hello ");
        cite_chord(&mut page);
        type_into(&mut page, "nothing like it");
        press_key(&mut page, egui::Key::Enter);
        page.frame(Vec::new());

        // The first field has the keyboard, and the key follows it.
        type_into(&mut page, "L. Lamport");
        {
            let form = &mut page.app.refs.form.as_mut().unwrap().form;
            form.reference.year = "1994".to_string();
            form.reference.title = "How to write".to_string();
        }
        type_into(&mut page, ", J. Doe");
        page.frame(Vec::new());
        assert_eq!(page.app.refs.form.as_ref().unwrap().form.key, "lamport1994");
        assert!(page.sees("How to write"), "the preview");
        assert_eq!(only_text(&page), "hello ", "typing stays in the form");

        page.press("Add and cite");

        assert!(page.app.refs.form.is_none());
        assert_eq!(only_text(&page), "hello [lamport1994]");
        let saved = page
            .app
            .refs
            .list
            .get("lamport1994")
            .expect("listed at once");
        assert_eq!(saved.authors, ["L. Lamport", "J. Doe"]);
        assert!(reference_file(&page, "lamport1994").exists());

        assert!(page.app.session.undo());
        assert_eq!(only_text(&page), "hello ");
        assert!(page.app.refs.list.contains("lamport1994"), "only the text");
    }

    #[test]
    fn a_form_with_a_problem_cant_be_saved() {
        let mut page = page_in_notebook("");
        cite_chord(&mut page);
        type_into(&mut page, "nothing like it");
        press_key(&mut page, egui::Key::Enter);
        page.frame(Vec::new());
        page.frame(Vec::new());
        assert!(!page.can_press("Add and cite"));
        assert!(page.sees("It needs a key."));

        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.refs.form.is_some(), "it stays up");
    }

    #[test]
    fn saving_a_new_key_the_caret_is_on_leaves_the_text_alone() {
        let mut page = page_in_notebook("as in [lamport1994]");
        let history = page.app.session.history();
        cite_chord(&mut page);

        let window = page.app.refs.form.as_mut().expect("the form opens");
        assert_eq!(window.form.key, "lamport1994");
        window.form.reference.title = "How to write".to_string();
        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.refs.form.is_none());
        assert_eq!(only_text(&page), "as in [lamport1994]");
        assert_eq!(page.app.session.history(), history);
        assert!(page.app.refs.list.contains("lamport1994"));
    }

    #[test]
    fn a_key_the_form_was_changed_to_isnt_put_inside_the_one_the_caret_is_on() {
        let mut page = page_in_notebook("as in [lamport1994] so");
        let block_id = page.app.session.document().blocks[0].id;
        page.app.session.set_caret(crate::edit::Cursor {
            block_id,
            offset: "as in [lam".len(),
        });
        cite_chord(&mut page);

        let window = page.app.refs.form.as_mut().expect("the form opens");
        window.form.key = "other".to_string();
        window.form.reference.title = "T".to_string();
        press_key(&mut page, egui::Key::Enter);

        assert_eq!(only_text(&page), "as in [lamport1994][other] so");
    }

    #[test]
    fn a_known_key_under_the_caret_opens_for_editing() {
        let mut page = page_in_notebook("as in [knuth1981]");
        let history = page.app.session.history();
        cite_chord(&mut page);

        let window = page.app.refs.form.as_mut().expect("the form opens");
        assert_eq!(
            window.form.reference.title,
            "Breaking paragraphs into lines"
        );
        window.form.reference.title = "Another title".to_string();
        page.frame(Vec::new());
        page.press("Save");

        assert!(page.app.refs.form.is_none());
        assert_eq!(
            page.app.refs.list.get("knuth1981").unwrap().title,
            "Another title"
        );
        let file = std::fs::read_to_string(reference_file(&page, "knuth1981")).unwrap();
        assert!(file.contains("Another title"), "{file}");
        assert_eq!(page.app.session.history(), history, "no edit to the text");
    }

    #[test]
    fn text_that_only_looks_like_a_key_opens_the_search() {
        let mut page = page_in_notebook("[knuth1981]");
        let block_id = page.app.session.document().blocks[0].id;
        page.app
            .session
            .set_block_kind(block_id, BlockKind::Code { language: None });
        cite_chord(&mut page);

        assert!(page.app.refs.cite_search.is_some());
        assert!(page.app.refs.form.is_none());
    }

    #[test]
    fn a_failed_save_stays_in_the_window() {
        let mut page = page_in_notebook("[broken2020]");
        write_reference(&root_of(&page), "broken2020", "kind: nonsense\n");
        page.app.reload_references();
        cite_chord(&mut page);

        let window = page.app.refs.form.as_mut().expect("the form opens");
        window.form.reference.title = "T".to_string();
        press_key(&mut page, egui::Key::Enter);
        page.frame(Vec::new());

        let window = page.app.refs.form.as_ref().expect("still up");
        let failed = window.failed.as_deref().expect("it says why");
        assert!(failed.contains("damaged"), "{failed}");
        assert!(page.sees("damaged"));
    }

    #[test]
    fn enter_in_the_freeform_box_is_a_new_line() {
        let mut page = page_in_notebook("[letter1984]");
        cite_chord(&mut page);
        page.press("Freeform");
        page.frame(Vec::new());

        type_into(&mut page, "A letter");
        press_key(&mut page, egui::Key::Enter);
        type_into(&mut page, "and more");
        page.frame(Vec::new());

        let window = page.app.refs.form.as_ref().expect("still up");
        assert_eq!(window.form.reference.text, "A letter\nand more");

        page.press("Add and cite");
        let saved = page.app.refs.list.get("letter1984").unwrap();
        assert_eq!(saved.kind, Kind::Freeform);
        assert_eq!(saved.text, "A letter\nand more");
    }

    #[test]
    fn typing_in_the_form_never_reaches_the_document_and_escape_cancels_it() {
        let mut page = page_in_notebook("[lamport1994]");
        cite_chord(&mut page);
        assert!(page.app.refs.form.is_some());

        type_into(&mut page, "typed in the form");
        assert_eq!(only_text(&page), "[lamport1994]");

        press_key(&mut page, egui::Key::Escape);

        assert!(page.app.refs.form.is_none());
        assert!(!page.app.dialog_open());
        assert!(!reference_file(&page, "lamport1994").exists());
        assert_eq!(only_text(&page), "[lamport1994]");
    }

    #[test]
    fn help_being_up_leaves_the_search_its_keys() {
        let mut page = page_in_notebook("");
        write_reference(&root_of(&page), "smith2020", GOOD);
        page.app.reload_references();
        cite_chord(&mut page);
        page.app.show_help = true;
        page.frame(Vec::new());

        press_key(&mut page, egui::Key::ArrowDown);
        press_key(&mut page, egui::Key::Enter);

        assert_eq!(only_text(&page), "[smith2020]");
    }

    #[test]
    fn help_being_up_leaves_the_form_its_enter() {
        let mut page = noted_key_with_its_form();
        page.app.show_help = true;
        page.frame(Vec::new());

        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.refs.form.is_none());
        assert!(reference_file(&page, "lamport1994").exists());
    }

    #[test]
    fn help_being_up_leaves_a_rename_its_enter() {
        let mut page = page_in_notebook("see [knuth1981]");
        open_list(&mut page);
        ask_to_rename(&mut page, "plass1981");
        page.app.show_help = true;
        page.frame(Vec::new());

        press_key(&mut page, egui::Key::Enter);
        wait_for_rename(&mut page);

        assert_eq!(only_text(&page), "see [plass1981]");
    }

    #[test]
    fn the_references_window_says_why_it_wont_open_over_a_dialog() {
        let mut page = page_in_notebook("hello");
        page.app.toggle_jump();
        page.frame(Vec::new());

        page.app.open_references();

        assert!(page.app.refs.window.is_none());
        let notice = page.app.notice.clone().expect("it says so");
        assert!(notice.contains("References"), "{notice}");

        page.app.nb.close_lists();
        page.frame(Vec::new());
        page.app.notice = None;
        page.app.open_references();
        assert!(page.app.refs.window.is_some());
        assert_eq!(page.app.notice, None);

        // Asking again with it up is no complaint.
        page.app.open_references();
        assert_eq!(page.app.notice, None);
    }

    #[test]
    fn the_chord_is_ignored_over_a_dialog() {
        let mut page = page_in_notebook("hello");
        page.app.toggle_jump();
        cite_chord(&mut page);
        assert!(page.app.refs.cite_search.is_none());
        page.app.nb.close_lists();

        cite_chord(&mut page);
        assert!(page.app.refs.cite_search.is_some());
        cite_chord(&mut page);
        assert!(page.app.refs.cite_search.is_some(), "it doesn't toggle");
        assert!(page.app.refs.form.is_none());
    }

    #[test]
    fn the_chord_is_ignored_in_a_notes_box() {
        let mut page = page_in_notebook("hello");
        let id = page
            .app
            .session
            .add_annotation(AnnotationKind::Footnote)
            .unwrap();
        page.app.set_open_note(Some(id));
        page.frame(Vec::new());
        page.frame(Vec::new());

        cite_chord(&mut page);

        assert!(!page.app.refs.window_open());
    }

    #[test]
    fn a_right_click_offers_citing_in_a_notebook_only() {
        let secondary = |page: &mut Headless, pos: Pos2| {
            page.frame(vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                page.frame(vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }]);
            }
            page.frame(Vec::new());
        };
        let mut page = page_in_notebook("hello ");
        let pos = near_the_start(&page);
        secondary(&mut page, pos);
        page.press("Cite…");
        page.frame(Vec::new());
        assert!(page.app.refs.cite_search.is_some());

        let folder = test_temp_dir().join(format!("outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let mut session = Session::open(&folder.join("doc.kame")).unwrap();
        session.insert("hello");
        let mut outside = Headless::new(session);
        outside.frame(Vec::new());
        secondary(&mut outside, pos);
        assert!(outside.sees("Footnote"), "the menu is up");
        assert!(!outside.sees("Cite…"));
    }

    #[test]
    fn the_entrys_edit_button_opens_the_form() {
        let mut page = page_in_notebook("[knuth1981] says so");
        page.click(near_the_start(&page));
        page.frame(Vec::new());
        assert!(page.app.refs.shown.is_some());

        page.press("Edit");

        assert_eq!(page.app.refs.shown, None);
        let window = page.app.refs.form.as_ref().expect("the form opens");
        assert_eq!(window.form.key, "knuth1981");
        assert!(!window.form.is_new());
    }

    #[test]
    fn the_list_shows_counts_and_load_problems() {
        let mut page = page_in_notebook("[knuth1981] and [knuth1981]");
        let root = root_of(&page);
        other_document(&page, "other.kame", "also [knuth1981]");
        write_reference(&root, "smith2020", GOOD);
        write_reference(&root, "broken2020", "kind: nonsense\n");
        page.app.reload_references();
        page.app.session.last_error = None;

        open_list(&mut page);

        assert_eq!(page.app.refs.counts.as_ref().unwrap()["knuth1981"], 2);
        assert!(page.sees("2 documents"));
        assert!(page.sees("0 documents"), "smith2020");
        assert!(
            page.sees("broken2020"),
            "what's wrong outlasts the status row"
        );
    }

    #[test]
    fn a_new_file_while_the_list_is_up_is_counted_again() {
        let mut page = page_in_notebook("[smith2020]");
        open_list(&mut page);
        assert!(!page
            .app
            .refs
            .counts
            .as_ref()
            .unwrap()
            .contains_key("smith2020"));

        write_reference(&root_of(&page), "smith2020", GOOD);
        page.app.refs.checked = None;
        run_until(&mut page, |app| {
            app.refs
                .counts
                .as_ref()
                .is_some_and(|counts| counts.contains_key("smith2020"))
        });
    }

    #[test]
    fn deleting_says_how_many_cite_it_and_shows_at_once() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);

        page.press("Delete…");
        page.frame(Vec::new());
        assert!(page.sees("1 document cites it."));
        assert!(reference_file(&page, "knuth1981").exists(), "only asked");

        page.press("Delete knuth1981");

        assert!(!page.app.refs.list.contains("knuth1981"), "at once");
        assert!(!reference_file(&page, "knuth1981").exists());
        assert_eq!(only_text(&page), "[knuth1981]");
        page.frame(Vec::new());
        assert!(page.sees("Deleted knuth1981"));
    }

    #[test]
    fn cancelling_a_confirmation_changes_nothing() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);

        page.press("Delete…");
        page.frame(Vec::new());
        page.press("Cancel");
        page.frame(Vec::new());

        assert!(!page.can_press("Delete knuth1981"));
        assert!(page.app.refs.list.contains("knuth1981"));
    }

    #[test]
    fn nothing_can_be_confirmed_before_the_count_is_known() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);
        page.press("Delete…");
        page.frame(Vec::new());
        let (_keep_it_waiting, coming) = channel();
        page.app.refs.counts = None;
        page.app.refs.counting = Some(coming);
        page.frame(Vec::new());
        page.frame(Vec::new());

        assert!(page.sees("Counting the documents that cite it…"));
        assert!(!page.can_press("Delete knuth1981"));
        assert!(page.sees("Delete knuth1981"));
    }

    #[test]
    fn escape_takes_the_windows_down_one_at_a_time() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);
        page.press("Edit");
        page.frame(Vec::new());
        assert!(page.app.refs.form.is_some());

        press_key(&mut page, egui::Key::Escape);
        assert!(page.app.refs.form.is_none());
        assert!(page.app.refs.window.is_some());

        page.press("Delete…");
        page.frame(Vec::new());
        press_key(&mut page, egui::Key::Escape);
        assert!(page.app.refs.window.as_ref().unwrap().confirming.is_none());
        assert!(page.app.refs.window.is_some());

        press_key(&mut page, egui::Key::Escape);
        assert!(page.app.refs.window.is_none());
        assert!(!page.app.dialog_open());
    }

    /// Asks to rename the only key, typing `new` as what it's to be.
    fn ask_to_rename(page: &mut Headless, new: &str) {
        page.press("Rename…");
        page.frame(Vec::new());
        page.frame(Vec::new());
        type_into(page, new);
        page.frame(Vec::new());
    }

    /// Waits for a rename, and for the list to be counted again after it.
    fn wait_for_rename(page: &mut Headless) {
        run_until(page, |app| {
            app.refs.renaming.is_none() && app.refs.counts.is_some()
        });
        page.frame(Vec::new());
    }

    /// Renames the only key to `new`, by way of its field and Enter.
    fn rename_only_key(page: &mut Headless, new: &str) {
        ask_to_rename(page, new);
        press_key(page, egui::Key::Enter);
        wait_for_rename(page);
    }

    #[test]
    fn the_rename_field_starts_empty_and_holds_only_whats_typed() {
        let mut page = page_in_notebook("see [knuth1981]");
        open_list(&mut page);
        let field = |page: &Headless| match &page.app.refs.window.as_ref().unwrap().confirming {
            Some(Confirming::Rename { new, .. }) => new.clone(),
            _ => panic!("not asking to rename"),
        };

        page.press("Rename…");
        page.frame(Vec::new());
        page.frame(Vec::new());
        assert_eq!(field(&page), "");
        assert!(!page.can_press("Rename knuth1981"));
        assert!(page.sees("Type the new key."));
        assert!(!page.sees("A key is letters"), "nothing wrong's been typed");

        type_into(&mut page, "plass1981");
        page.frame(Vec::new());
        assert_eq!(field(&page), "plass1981");
        assert!(page.can_press("Rename knuth1981"));
    }

    #[test]
    fn enter_in_the_form_doesnt_confirm_a_rename_behind_it() {
        let mut page = page_in_notebook("see [knuth1981]");
        open_list(&mut page);
        ask_to_rename(&mut page, "plass1981");

        let reference = page.app.refs.list.get("knuth1981").unwrap().clone();
        page.app
            .refs
            .open_form(ReferenceForm::editing("knuth1981", &reference));
        page.frame(Vec::new());
        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.refs.renaming.is_none());
        assert!(reference_file(&page, "knuth1981").exists());
    }

    #[test]
    fn the_rename_button_renames_as_enter_does() {
        let mut page = page_in_notebook("see [knuth1981]");
        open_list(&mut page);
        ask_to_rename(&mut page, "plass1981");

        page.press("Rename knuth1981");
        wait_for_rename(&mut page);

        assert_eq!(only_text(&page), "see [plass1981]");
    }

    #[test]
    fn a_rename_to_nothing_new_cant_be_confirmed() {
        let mut page = page_in_notebook("see [knuth1981]");
        open_list(&mut page);

        ask_to_rename(&mut page, "knuth1981");
        assert!(!page.can_press("Rename knuth1981"));

        ask_to_rename(&mut page, "not a key");
        assert!(!page.can_press("Rename knuth1981"));
        press_key(&mut page, egui::Key::Enter);
        assert!(page.app.refs.renaming.is_none());
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

        let done = rename(&notebook, "knuth1981", "plass1981", Some(&open));

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

    #[cfg(unix)]
    #[test]
    fn an_old_file_that_cant_be_removed_at_the_end_is_named_and_a_retry_finishes() {
        use std::os::unix::fs::PermissionsExt;
        let mut page = page_in_notebook("see [knuth1981]");
        let notebook = page.app.nb.notebook.clone().unwrap();
        let open = page.app.session.path().unwrap().to_path_buf();
        open_list(&mut page);
        let send = rename_held_open(&mut page);
        let done = rename(&notebook, "knuth1981", "plass1981", Some(&open));
        let folder = root_of(&page).join("references");

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
    fn an_unsaved_copy_that_cant_follow_says_the_old_key_has_gone() {
        let mut page = imported_markdown("No citation on disk.\n");
        page.app.session.insert(" [knuth1981]");
        open_list(&mut page);
        page.app.session.save_failed = true;
        page.app.session.last_error = Some("the disk is full".to_string());

        rename_only_key(&mut page, "plass1981");

        assert!(!reference_file(&page, "knuth1981").exists());
        assert!(page.sees("this document: the disk is full"));
        assert!(page.sees("knuth1981 has gone"));
    }

    #[test]
    fn an_open_document_from_a_newer_version_keeps_the_old_key_too() {
        let mut page = page_in_notebook("see [knuth1981]");
        open_list(&mut page);
        page.app.session.newer = true;

        rename_only_key(&mut page, "plass1981");

        assert!(reference_file(&page, "knuth1981").exists());
        assert_eq!(only_text(&page), "see [knuth1981]");
        assert!(page.sees("doc.kame: its citations could not be rewritten"));
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

        page.app.open_document(other.display().to_string());
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
        page.app.open_document(other.display().to_string());
        assert_eq!(page.app.session.path(), Some(other.as_path()));
        assert!(
            page.app.refs.window.is_none(),
            "another document's windows are its own"
        );
    }

    /// Selects `start..end` of the first block, with the caret at `caret`.
    fn select(page: &mut Headless, start: usize, end: usize, caret: usize) {
        let block_id = page.app.session.document().blocks[0].id;
        let at = |offset| crate::edit::Cursor { block_id, offset };
        page.app.session.set_caret(at(caret));
        let (from, to) = if caret == start {
            (end, start)
        } else {
            (start, end)
        };
        page.app.session.selection = Some(crate::edit::Selection {
            start: at(from),
            end: at(to),
        });
    }

    fn ctrl_k(page: &mut Headless) {
        let modifiers = egui::Modifiers {
            command: true,
            ..egui::Modifiers::NONE
        };
        key(page, egui::Key::K, modifiers);
        page.frame(Vec::new());
    }

    /// A rename that never finishes until `send` says so, of `knuth1981`.
    fn rename_held_open(page: &mut Headless) -> std::sync::mpsc::Sender<Result<Renamed, String>> {
        let (send, coming) = channel();
        page.app.refs.renaming = Some(Renaming {
            coming,
            notebook: page.app.nb.notebook.clone().unwrap(),
            open: page.app.session.path().map(Path::to_path_buf),
            old: "knuth1981".to_string(),
            new: "plass1981".to_string(),
        });
        send
    }

    /// The form for a new reference, over a page that has `[knuth1981]` near its start, with
    /// something typed in it.
    fn typed_form_over_a_citation() -> Headless {
        let mut page = page_in_notebook("[knuth1981] says so ");
        cite_chord(&mut page);
        type_into(&mut page, "zzz");
        press_key(&mut page, egui::Key::Enter);
        page.frame(Vec::new());
        page.frame(Vec::new());
        type_into(&mut page, "Typed Author");
        page.frame(Vec::new());
        assert_eq!(typed_authors(&page), "Typed Author");
        page
    }

    fn typed_authors(page: &Headless) -> String {
        let window = page.app.refs.form.as_ref().expect("the form is up");
        window.form.authors_text.clone()
    }

    #[test]
    fn citing_over_a_selection_keeps_the_words_and_one_undo_takes_only_the_key() {
        for caret in [10, 5] {
            let mut page = page_in_notebook("keep these words here");
            select(&mut page, 5, 10, caret);
            cite_chord(&mut page);
            assert!(page.app.refs.cite_search.is_some());

            press_key(&mut page, egui::Key::Enter);

            assert_eq!(only_text(&page), "keep these[knuth1981] words here");
            assert_eq!(page.app.session.selection, None);
            assert!(page.app.session.undo());
            assert_eq!(
                only_text(&page),
                "keep these words here",
                "caret at {caret}"
            );
        }
    }

    #[test]
    fn add_and_cite_over_a_selection_keeps_the_words_too() {
        let mut page = page_in_notebook("keep these words here");
        select(&mut page, 5, 10, 10);
        cite_chord(&mut page);
        type_into(&mut page, "lamport1994");
        page.frame(Vec::new());
        press_key(&mut page, egui::Key::Enter);
        page.frame(Vec::new());
        page.app.refs.form.as_mut().unwrap().form.reference.title = "T".to_string();
        page.frame(Vec::new());

        page.press("Add and cite");

        assert_eq!(only_text(&page), "keep these[lamport1994] words here");
        assert!(page.app.session.undo());
        assert_eq!(only_text(&page), "keep these words here");
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
            .save_as(&root_of(&page).join("copy.kame"))
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
            .save_as(&outside.join("copy.kame"))
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
        send.send(rename(&notebook, "knuth1981", "plass1981", Some(&open)))
            .unwrap();
        run_until(&mut page, |app| app.refs.renaming.is_none());
        page.frame(Vec::new());

        assert!(page.sees("knuth1981 was kept"));
        assert!(root.join("references/knuth1981.yaml").exists());
        assert_eq!(text_on_disk(&open), "see [knuth1981]");
    }

    #[test]
    fn enter_in_a_link_prompt_doesnt_start_the_rename_behind_it() {
        let mut page = page_in_notebook("hello [knuth1981]");
        select(&mut page, 0, 5, 5);
        open_list(&mut page);
        ask_to_rename(&mut page, "plass1981");
        ctrl_k(&mut page);
        assert!(page.app.link_prompt.is_some());

        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.link_prompt.is_none(), "the prompt had it");
        assert!(page.app.refs.renaming.is_none());
        assert!(reference_file(&page, "knuth1981").exists());
        assert!(!reference_file(&page, "plass1981").exists());
    }

    #[test]
    fn enter_in_a_link_prompt_doesnt_save_the_form_behind_it() {
        let mut page = page_in_notebook("hello [lamport1994]");
        select(&mut page, 0, 5, 5);
        // The caret's on nothing, so reach the form on the key by hand.
        page.app
            .refs
            .open_form(ReferenceForm::new(Some("lamport1994".to_string())));
        page.app.refs.form.as_mut().unwrap().form.reference.title = "T".to_string();
        page.frame(Vec::new());
        ctrl_k(&mut page);
        assert!(page.app.link_prompt.is_some());

        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.link_prompt.is_none(), "the prompt had it");
        assert!(page.app.refs.form.is_some());
        assert!(!reference_file(&page, "lamport1994").exists());
    }

    #[test]
    fn enter_in_the_properties_prompt_doesnt_save_the_form_behind_it() {
        let mut page = page_in_notebook("[lamport1994]");
        cite_chord(&mut page);
        page.app.refs.form.as_mut().unwrap().form.reference.title = "T".to_string();
        page.app.begin_properties();
        page.frame(Vec::new());
        page.frame(Vec::new());

        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.metadata_prompt.is_none(), "the prompt had it");
        assert!(page.app.refs.form.is_some());
        assert!(!reference_file(&page, "lamport1994").exists());
    }

    #[test]
    fn enter_in_a_link_prompt_doesnt_choose_from_the_search_behind_it() {
        let mut page = page_in_notebook("hello ");
        select(&mut page, 0, 5, 5);
        cite_chord(&mut page);
        ctrl_k(&mut page);
        assert!(page.app.link_prompt.is_some());

        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.link_prompt.is_none());
        assert!(page.app.refs.cite_search.is_some());
        assert_eq!(only_text(&page), "hello ");
    }

    #[test]
    fn enter_in_the_find_bar_doesnt_save_the_form_behind_it() {
        let mut page = page_in_notebook("[lamport1994]");
        cite_chord(&mut page);
        page.app.refs.form.as_mut().unwrap().form.reference.title = "T".to_string();
        let ctrl = egui::Modifiers {
            command: true,
            ..egui::Modifiers::NONE
        };
        key(&mut page, egui::Key::F, ctrl);
        assert!(page.app.find.is_some());
        // The find bar is in the chrome, which isn't drawn here. It takes the keyboard as it opens.
        page.ctx
            .memory_mut(|memory| memory.request_focus(crate::find_query_id()));

        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.refs.form.is_some());
        assert!(!reference_file(&page, "lamport1994").exists());
    }

    #[test]
    fn a_find_bar_left_up_doesnt_take_enter_from_a_form_opened_after_it() {
        let mut page = page_in_notebook("[lamport1994]");
        let ctrl = egui::Modifiers {
            command: true,
            ..egui::Modifiers::NONE
        };
        key(&mut page, egui::Key::F, ctrl);
        page.frame(Vec::new());
        page.click(near_the_start(&page));
        page.frame(Vec::new());
        let end = only_text(&page).len();
        let block_id = page.app.session.document().blocks[0].id;
        page.app.session.set_caret(crate::edit::Cursor {
            block_id,
            offset: end,
        });

        cite_chord(&mut page);
        page.app
            .refs
            .form
            .as_mut()
            .expect("the form opens")
            .form
            .reference
            .title = "T".to_string();
        page.frame(Vec::new());
        press_key(&mut page, egui::Key::Enter);

        assert!(page.app.find.is_some());
        assert!(reference_file(&page, "lamport1994").exists());
    }

    #[test]
    fn a_click_on_a_citation_behind_a_form_leaves_the_form_alone() {
        let mut page = typed_form_over_a_citation();

        page.click(near_the_start(&page));
        page.frame(Vec::new());

        assert_eq!(page.app.refs.shown, None);
        assert_eq!(typed_authors(&page), "Typed Author");
    }

    #[test]
    fn a_click_on_a_citation_during_a_rename_opens_nothing() {
        let mut page = page_in_notebook("[knuth1981] says so");
        open_list(&mut page);
        let _send = rename_held_open(&mut page);

        page.click(near_the_start(&page));
        page.frame(Vec::new());

        assert_eq!(page.app.refs.shown, None);
        assert!(page.app.refs.form.is_none());
    }

    #[test]
    fn a_click_on_a_link_behind_a_form_stays_where_it_is() {
        let mut page = page_in_notebook("other says so ");
        other_document(&page, "other.kame", "words");
        let block_id = page.app.session.document().blocks[0].id;
        assert!(page
            .app
            .session
            .edit_link(block_id, 0, 5, "other", "other.kame"));
        let here = page.app.session.path().unwrap().to_path_buf();
        page.frame(Vec::new());
        cite_chord(&mut page);
        press_key(&mut page, egui::Key::Escape);
        page.app.refs.open_form(ReferenceForm::new(None));
        page.frame(Vec::new());

        page.click(near_the_start(&page));
        page.frame(Vec::new());

        assert_eq!(page.app.session.path(), Some(here.as_path()));
        assert!(page.app.refs.form.is_some());

        // Nor does a right-click offer to follow it.
        let pos = near_the_start(&page);
        page.frame(vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            page.frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }]);
        }
        assert_eq!(page.app.link_menu, None);

        // With nothing up, the same click does follow it.
        press_key(&mut page, egui::Key::Escape);
        press_key(&mut page, egui::Key::Escape);
        page.click(near_the_start(&page));
        page.frame(Vec::new());
        assert_ne!(page.app.session.path(), Some(here.as_path()));
    }

    #[test]
    fn a_click_on_what_links_here_behind_a_form_stays_where_it_is() {
        let root = notebook_folder("linked-to");
        let mut linking = Session::open(&root.join("linking.kame")).unwrap();
        linking.insert("to it");
        let block_id = linking.document().blocks[0].id;
        assert!(linking.edit_link(block_id, 0, 2, "to", "doc.kame"));
        drop(linking);
        let mut session = Session::open(&root.join("doc.kame")).unwrap();
        session.insert("words");
        let mut page = Headless::new(session);
        run_until(&mut page, |app| !app.nb.backlinks.is_empty());
        let x = page.body_left() + 8.0;
        let click_down_the_page = |page: &mut Headless| {
            for y in (20..780).step_by(8) {
                page.click(Pos2::new(x, y as f32));
            }
        };

        page.app.refs.open_form(ReferenceForm::new(None));
        page.frame(Vec::new());
        click_down_the_page(&mut page);
        assert_eq!(page.app.nb.pending_open, None);
        assert!(page.app.refs.form.is_some());

        // With nothing up, one of those clicks is on it.
        press_key(&mut page, egui::Key::Escape);
        click_down_the_page(&mut page);
        assert_ne!(page.app.nb.pending_open, None);
    }

    #[test]
    fn a_key_that_cant_be_put_in_the_document_says_so() {
        let mut page = page_in_notebook("hello ");
        cite_chord(&mut page);
        page.app.session.save_failed = true;
        page.app.session.last_error = Some("the disk is full".to_string());

        press_key(&mut page, egui::Key::Enter);

        assert_eq!(only_text(&page), "hello ");
        let said = page.app.session.last_error.as_deref().unwrap();
        assert!(said.contains("knuth1981"), "{said}");
        assert!(said.contains("the disk is full"), "{said}");
    }

    #[test]
    fn a_key_chosen_on_the_same_text_in_code_is_still_put_in() {
        let mut page = page_in_notebook("[knuth1981]");
        let block_id = page.app.session.document().blocks[0].id;
        page.app
            .session
            .set_block_kind(block_id, BlockKind::Code { language: None });
        cite_chord(&mut page);
        assert!(page.app.refs.cite_search.is_some());

        press_key(&mut page, egui::Key::Enter);

        assert_eq!(only_text(&page), "[knuth1981][knuth1981]");
    }

    #[test]
    fn escape_closes_an_entry_over_the_list_and_leaves_the_list() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);
        page.press("Delete…");
        page.frame(Vec::new());
        page.app.refs.show("knuth1981", Pos2::new(10.0, 10.0));
        page.frame(Vec::new());

        press_key(&mut page, egui::Key::Escape);

        assert_eq!(page.app.refs.shown, None);
        let window = page.app.refs.window.as_ref().expect("the list stays");
        assert!(window.confirming.is_some(), "and so does what it's asking");
    }

    #[test]
    fn escape_closes_a_prompt_over_a_form_and_leaves_the_form() {
        let mut page = typed_form_over_a_citation();
        select(&mut page, 0, 3, 3);
        ctrl_k(&mut page);
        assert!(page.app.link_prompt.is_some());

        press_key(&mut page, egui::Key::Escape);

        assert!(page.app.link_prompt.is_none());
        assert_eq!(typed_authors(&page), "Typed Author");
    }

    #[test]
    fn escape_closes_a_prompt_over_the_search_and_leaves_the_search() {
        let mut page = page_in_notebook("hello ");
        select(&mut page, 0, 5, 5);
        cite_chord(&mut page);
        ctrl_k(&mut page);

        press_key(&mut page, egui::Key::Escape);

        assert!(page.app.link_prompt.is_none());
        assert!(page.app.refs.cite_search.is_some());
    }

    #[test]
    fn a_rename_says_why_its_button_is_off() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);

        ask_to_rename(&mut page, "not a key");
        page.frame(Vec::new());
        assert!(!page.can_press("Rename knuth1981"));
        assert!(page.sees("A key is letters"));

        ask_to_rename(&mut page, "knuth1981");
        page.frame(Vec::new());
        assert!(!page.can_press("Rename knuth1981"));
        assert!(page.sees("Type the new key."));
    }

    #[test]
    fn a_confirmation_reads_right_with_nothing_citing_the_key() {
        let mut page = page_in_notebook("no citations here");
        open_list(&mut page);

        page.press("Delete…");
        page.frame(Vec::new());
        assert!(page.sees("No document cites it."));
        assert!(!page.sees("hold plain"));

        page.press("Cancel");
        ask_to_rename(&mut page, "plass1981");
        assert!(page.sees("No document cites it."));
        assert!(!page.sees("rewritten"));
    }

    #[test]
    fn a_confirmation_reads_right_with_one_document_citing_the_key() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);

        page.press("Delete…");
        page.frame(Vec::new());
        assert!(page.sees("1 document cites it. It would then hold plain [knuth1981] text."));
    }

    #[test]
    fn a_count_that_stopped_can_still_be_gone_ahead_with() {
        let mut page = page_in_notebook("[knuth1981]");
        open_list(&mut page);
        page.press("Delete…");
        page.frame(Vec::new());

        let (send, coming) = channel();
        drop(send);
        page.app.refs.counts = None;
        page.app.refs.counting = Some(coming);
        page.frame(Vec::new());
        page.frame(Vec::new());

        assert!(page.sees("couldn't be counted"));
        assert!(page.can_press("Delete knuth1981"));
        page.press("Delete knuth1981");
        assert!(!reference_file(&page, "knuth1981").exists());
    }

    /// A page with the browser up and its references showing.
    fn page_with_browser(text: &str) -> Headless {
        let mut page = page_in_notebook(text);
        page.app.nb.browser_open = true;
        page.frame(Vec::new());
        page.press("▸ References");
        page.frame(Vec::new());
        page
    }

    /// A reference typed into the form that's up.
    fn fill_form(page: &mut Headless, key: &str) {
        page.frame(Vec::new());
        let form = &mut page.app.refs.form.as_mut().unwrap().form;
        form.key = key.to_string();
        form.key_edited = true;
        form.reference.title = "How to write".to_string();
        page.frame(Vec::new());
    }

    #[test]
    fn the_browser_keeps_its_references_under_a_row_that_opens() {
        let mut page = page_in_notebook("");
        page.app.nb.browser_open = true;
        page.frame(Vec::new());
        assert!(page.can_press("▸ References"));
        assert!(!page.sees("knuth1981"), "closed as a folder starts");

        page.press("▸ References");
        page.frame(Vec::new());

        assert!(page.can_press("▾ References"));
        assert!(page.can_press("knuth1981"));
        assert!(page.can_press("New reference…"));

        page.press("▾ References");
        page.frame(Vec::new());
        assert!(!page.sees("knuth1981"));
    }

    #[test]
    fn a_browser_with_no_references_can_still_add_one() {
        let root = notebook_folder("no-references");
        let session = Session::open(&root.join("doc.kame")).unwrap();
        let mut page = Headless::new(session);
        page.app.nb.browser_open = true;
        page.frame(Vec::new());

        page.press("▸ References");
        page.frame(Vec::new());

        assert!(page.can_press("New reference…"));
    }

    #[test]
    fn the_browser_remembers_its_references_as_it_does_a_folder() {
        let page = page_with_browser("");
        assert!(page.app.nb.browser_folders.contains(BROWSER_SECTION));
        // A folder's name is relative to the notebook, so none can be this.
        assert!(BROWSER_SECTION.starts_with('/'));
    }

    #[test]
    fn choosing_a_reference_in_the_browser_opens_it_to_edit() {
        let mut page = page_with_browser("hello");

        page.press("knuth1981");
        page.frame(Vec::new());

        assert!(page.sees("Edit reference"));
        assert!(page.sees("Breaking paragraphs into lines"));
        page.app.refs.form.as_mut().unwrap().form.reference.title = "Changed".to_string();
        page.press("Save");

        assert_eq!(
            page.app.refs.list.get("knuth1981").unwrap().title,
            "Changed"
        );
        assert_eq!(only_text(&page), "hello");
    }

    #[test]
    fn adding_from_the_browser_leaves_the_document_alone() {
        let mut page = page_with_browser("hello ");
        let history = page.app.session.history();

        page.press("New reference…");
        fill_form(&mut page, "lamport1994");
        assert!(page.can_press("Add"));
        assert!(!page.can_press("Add and cite"));
        page.press("Add");

        assert!(page.app.refs.form.is_none());
        assert!(page.app.refs.list.contains("lamport1994"), "listed at once");
        assert!(reference_file(&page, "lamport1994").exists());
        assert_eq!(only_text(&page), "hello ");
        assert_eq!(page.app.session.history(), history, "no edit was made");
        page.frame(Vec::new());
        assert!(page.can_press("lamport1994"), "and in the browser");
    }

    #[test]
    fn enter_adds_from_the_browser_without_citing_too() {
        let mut page = page_with_browser("hello ");

        page.press("New reference…");
        fill_form(&mut page, "lamport1994");
        press_key(&mut page, egui::Key::Enter);

        assert!(reference_file(&page, "lamport1994").exists());
        assert_eq!(only_text(&page), "hello ");
    }

    #[test]
    fn a_failed_add_from_the_browser_can_still_be_added_without_citing() {
        let mut page = page_with_browser("hello ");
        let folder = root_of(&page).join("references");
        page.press("New reference…");
        fill_form(&mut page, "lamport1994");

        std::fs::remove_dir_all(&folder).unwrap();
        std::fs::write(&folder, "a file in the way").unwrap();
        page.press("Add");
        assert!(page.app.refs.form.is_some(), "stays up to say why");

        std::fs::remove_file(&folder).unwrap();
        std::fs::create_dir(&folder).unwrap();
        page.frame(Vec::new());
        assert!(page.can_press("Add"));
        page.press("Add");

        assert!(reference_file(&page, "lamport1994").exists());
        assert_eq!(only_text(&page), "hello ");
    }

    #[test]
    fn citing_still_adds_and_cites_once_the_browser_has_added() {
        let mut page = page_with_browser("hello ");
        page.press("New reference…");
        fill_form(&mut page, "lamport1994");
        page.press("Add");

        cite_chord(&mut page);
        type_into(&mut page, "plass");
        page.frame(Vec::new());
        page.press("New reference “plass”…");
        fill_form(&mut page, "plass1981");
        assert!(page.can_press("Add and cite"));
        page.press("Add and cite");

        assert_eq!(only_text(&page), "hello [plass1981]");
    }

    #[test]
    fn deleting_from_the_browser_asks_first() {
        let mut page = page_with_browser("[knuth1981]");

        page.right_click("knuth1981");
        page.press("Delete…");
        run_until(&mut page, |app| app.refs.counts.is_some());
        page.frame(Vec::new());

        assert!(page.sees("1 document cites it."));
        assert!(reference_file(&page, "knuth1981").exists(), "only asked");

        page.press("Delete knuth1981");

        assert!(!reference_file(&page, "knuth1981").exists());
        assert_eq!(only_text(&page), "[knuth1981]");
    }

    #[test]
    fn renaming_from_the_browser_asks_for_the_new_key() {
        let mut page = page_with_browser("see [knuth1981]");

        page.right_click("knuth1981");
        page.press("Rename…");
        run_until(&mut page, |app| app.refs.counts.is_some());
        page.frame(Vec::new());
        page.frame(Vec::new());
        type_into(&mut page, "plass1981");
        page.frame(Vec::new());

        page.press("Rename knuth1981");
        wait_for_rename(&mut page);

        assert_eq!(only_text(&page), "see [plass1981]");
        assert!(reference_file(&page, "plass1981").exists());
    }

    #[test]
    fn a_confirmation_asked_from_the_browser_scrolls_into_view() {
        let mut page = page_in_notebook("");
        let root = root_of(&page);
        for at in 0..40 {
            write_reference(&root, &format!("zed{at:02}"), GOOD);
        }
        page.app.reload_references();
        page.app.nb.browser_open = true;
        page.frame(Vec::new());
        page.app
            .browser_reference(BrowserReference::Delete("zed39".to_string()));
        run_until(&mut page, |app| app.refs.counts.is_some());
        for _ in 0..4 {
            page.frame(Vec::new());
        }

        let (_, bottom) = page.drawn_at("Delete zed39").expect("the button is drawn");
        let (close_top, _) = page.drawn_at("Close").expect("the window's button");
        assert!(
            bottom <= close_top,
            "{bottom} is below the list, at {close_top}"
        );
    }

    #[test]
    fn the_browser_opens_the_form_over_the_references_window() {
        let mut page = page_with_browser("");
        page.app.open_references();
        run_until(&mut page, |app| app.refs.counts.is_some());

        page.app
            .browser_reference(BrowserReference::Edit("knuth1981".to_string()));
        page.frame(Vec::new());
        page.frame(Vec::new());

        assert!(page.sees("Edit reference"));
        press_key(&mut page, egui::Key::Escape);
        assert!(page.app.refs.form.is_none());
        assert!(page.app.refs.window.is_some(), "back to the list");
    }

    #[test]
    fn the_browser_doesnt_take_away_a_form_being_typed_in() {
        let mut page = page_with_browser("");
        page.press("New reference…");
        fill_form(&mut page, "lamport1994");

        page.press("knuth1981");

        let form = &page.app.refs.form.as_ref().expect("still up").form;
        assert_eq!(form.key, "lamport1994");
        assert!(form.is_new());
        assert_eq!(
            page.app.notice.as_deref(),
            Some("Close what's open, then choose that again.")
        );
    }

    #[test]
    fn the_browser_waits_for_a_rename_under_way() {
        let mut page = page_with_browser("");
        page.app.open_references();
        let (_send, coming) = channel();
        let notebook = page.app.nb.notebook.clone().unwrap();
        page.app.refs.renaming = Some(Renaming {
            coming,
            notebook,
            open: None,
            old: "knuth1981".to_string(),
            new: "plass1981".to_string(),
        });

        page.app.browser_reference(BrowserReference::New);

        assert!(page.app.refs.form.is_none());
        assert_eq!(
            page.app.notice.as_deref(),
            Some("Close what's open, then choose that again.")
        );
    }

    #[test]
    fn the_browser_waits_behind_another_dialog() {
        let mut page = page_with_browser("");
        page.app.nb.move_report = Some(Default::default());

        page.app
            .browser_reference(BrowserReference::Delete("knuth1981".to_string()));

        assert!(page.app.refs.window.is_none());
    }

    #[test]
    fn the_browser_doesnt_take_away_a_search_being_typed_in() {
        let mut page = page_with_browser("");
        cite_chord(&mut page);
        type_into(&mut page, "knu");

        page.app.browser_reference(BrowserReference::New);

        assert!(page.app.refs.form.is_none());
        assert_eq!(page.app.refs.cite_search.as_ref().unwrap().query, "knu");
    }

    #[test]
    fn the_browser_waits_for_the_close_prompt() {
        let mut page = page_with_browser("");
        page.app.close_flow.prompting = true;

        page.app.browser_reference(BrowserReference::New);

        assert!(page.app.refs.form.is_none());
    }

    #[test]
    fn asking_from_the_browser_counts_again() {
        let mut page = page_with_browser("");
        open_list(&mut page);
        page.press("Close");
        other_document(&page, "other.kame", "see [knuth1981]");

        page.app
            .browser_reference(BrowserReference::Delete("knuth1981".to_string()));
        page.frame(Vec::new());
        run_until(&mut page, |app| app.refs.counts.is_some());
        page.frame(Vec::new());

        assert!(page.sees("1 document cites it."));
    }

    #[test]
    fn the_references_window_can_add_without_citing() {
        let mut page = page_in_notebook("hello ");
        open_list(&mut page);

        page.press("New reference…");
        fill_form(&mut page, "lamport1994");
        page.press("Add");

        assert!(page.app.refs.window.is_some(), "the list stays up");
        assert_eq!(only_text(&page), "hello ");
        page.frame(Vec::new());
        assert!(page.sees("lamport1994"), "and shows it");
    }

    #[test]
    fn an_empty_references_window_says_how_to_add_one() {
        let root = notebook_folder("empty-window");
        let session = Session::open(&root.join("doc.kame")).unwrap();
        let mut page = Headless::new(session);
        page.frame(Vec::new());
        open_list(&mut page);

        assert!(page.sees("No references yet."));
        assert!(!page.sees("Ctrl-Shift-C"), "the button is right there");
        assert!(page.can_press("New reference…"));
    }
}
