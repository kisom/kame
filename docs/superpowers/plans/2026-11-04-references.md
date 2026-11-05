# References Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cite a notebook's sources by typing `[key]`, enter them inside kame, and end exports with an IEEE-format References section of what's cited.

**Architecture:** A citation is only text, recognized wherever it's needed by a pure function in `kame-core`. A notebook keeps one YAML file per reference, read and written by `kame-notebook`. Exports and `kame-site` take the list as an argument, and the page draws known keys from a throwaway copy of each block with link spans added.

**Tech Stack:** Rust workspace, egui 0.37, `serde`, `yaml_serde` (already in `kame-notebook`). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-04-references-design.md`. Read it first. It holds the IEEE forms, the example entries and the rules for where a citation counts.

## Global Constraints

- All offsets are UTF-8 byte offsets. Text changes go through existing `Session` edits, never around them.
- The document format doesn't change. No new field in `Document`, `Block`, `Metadata` or `SpanStyle`, and no change to any digest.
- A document that cites nothing exports byte-identically to today, in HTML and Markdown.
- Export is deterministic: output depends only on the document and the reference contents.
- Layering is one-way. `kame-core` knows nothing of notebooks, typeset or the app, and logic stays out of the app where it can go in a library.
- Keys: ASCII letters, digits, `-` and `_`, starting with a letter, at most 64 bytes.
- Offline: nothing here touches the network.
- Gate before every commit: `cargo build --all-targets`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`. Commit only the files the task changed, with `Co-Authored-By` and `Claude-Session` trailers as the session instructs.
- Prose in docs and comments follows the repo's plain style. Test names read as sentences, as in the neighbouring tests.

## Review Focus

1. A hand-edited file with `year: 1981` or `volume: 11` unquoted should load as text rather than being refused (Task 4).
2. A `references/` folder that already holds documents, plus a Syncthing conflict copy in it: documents stay listed, the copy is ignored and never loads as a reference (Task 4).
3. Citations against punctuation and multibyte text, `[smith2020].`, `é[a]é`, `[a][b]`, in bold or a highlight, must keep offsets right and nest valid HTML (Tasks 1 and 3).
4. Hostile reference text, `<script>` in a title and `javascript:` as a URL in a site build, must be escaped and never linked. A freeform entry with newlines mustn't break Markdown (Task 3).
5. Renaming a key in a document inside a `.kame-logs` folder must survive that document's next open and sync. `[old2]`, `[old]` in code and `[old]` in a note stay as they are (Task 5).

---

### Task 1: Citations in `kame-core`

**Files:**
- Create: `crates/kame-core/src/references/mod.rs`
- Modify: `crates/kame-core/src/lib.rs` (add `pub mod references;`)

**Interfaces:**
- Produces, all in `kame_core::references`:
  - `Reference` and `Kind`, below.
  - `pub struct References(BTreeMap<String, Reference>)` with `const fn new()`, `get`, `contains`, `insert`, `remove`, `len`, `is_empty`, `keys`, and `sorted() -> Vec<(&str, &Reference)>` (by key ignoring ASCII case, then bytes).
  - `pub fn valid_key(key: &str) -> bool`
  - `pub fn citation_ranges(text: &str) -> Vec<(usize, usize)>`: byte ranges including the brackets.
  - `pub fn block_citation_ranges(block: &Block) -> Vec<(usize, usize)>`
  - `pub fn key_of(text: &str, range: (usize, usize)) -> &str`
  - `pub fn citation_url(key: &str) -> String` (`#ref-KEY`) and `pub fn cited_key(url: &str) -> Option<&str>`
  - `pub fn with_citation_spans<'a>(block: &'a Block, references: &References) -> Cow<'a, Block>`
  - `pub fn cited<'a>(doc: &Document, references: &'a References, marginals: bool) -> Vec<(&'a str, &'a Reference)>` in `sorted()` order
  - `pub fn unresolved(doc: &Document, references: &References, marginals: bool) -> Vec<String>`: sorted, deduplicated
  - `pub fn looks_like_suggested_key(key: &str) -> bool`
  - `pub fn rewrite_citation(text: &str, old: &str, new: &str) -> String`
  - `pub fn citation_at(text: &str, offset: usize) -> Option<(usize, usize)>`: the citation-shaped range the offset is in or just after
  - `pub fn suggest_key(reference: &Reference, references: &References) -> Option<String>`

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind { #[default] Article, Conference, Book, Web, Freeform }

/// One flat struct, so switching kinds in the form keeps what was typed.
/// Every field but `kind` has serde default and skip-if-empty. Strings also accept a YAML number.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub kind: Kind,
    pub authors: Vec<String>, pub editors: Vec<String>,
    pub title: String, pub chapter: String, pub published_in: String, pub location: String,
    pub edition: String, pub place: String, pub publisher: String, pub site: String,
    pub volume: String, pub number: String, pub pages: String, pub month: String,
    pub year: String, pub doi: String, pub url: String, pub accessed: String, pub text: String,
}
impl Reference {
    /// `Err` names a field the kind doesn't have (per the spec's table), or a missing `title` / `text`.
    pub fn check(&self) -> Result<(), String>;
}
```

- [ ] **Step 1: Write the failing tests**, in the module:

```rust
#[test] fn a_key_is_ascii_starting_with_a_letter() {
    for good in ["smith2020", "a", "a-b_c", "A1"] { assert!(valid_key(good), "{good}"); }
    for bad in ["", "1", "2020", "-a", "a b", "é", "a.b", "a:b", &"a".repeat(65)] { assert!(!valid_key(bad), "{bad}"); }
}
#[test] fn only_a_whole_key_in_single_brackets_is_a_citation() {
    assert_eq!(citation_ranges("see [a1]."), vec![(4, 8)]);
    assert_eq!(citation_ranges("é[a]é"), vec![(2, 5)]);
    assert_eq!(citation_ranges("[a][b]"), vec![(0, 3), (3, 6)]);
    for none in ["[[a]]", "[a](b)", "[1]", "[a b]", "[]", "[a", "a]", "\\"] { assert_eq!(citation_ranges(none), vec![], "{none}"); }
}
```

Also: `block_citation_ranges` is empty in code and prose blocks and images, and skips ranges overlapping `Code`, `Link` and `DocumentLink` spans. `cited` sees blocks and footnotes, sees marginals only when asked, never notes, ignores unknown keys, lists each key once, and sorts `Apple`, `banana`, `Cherry`. `unresolved` reports `[smith2020]` and `[smith2020a]`, not `[sic]`, and not a known key. `rewrite_citation("[old] [old2] [[old]]", "old", "new")` gives `"[new] [old2] [[old]]"`. `with_citation_spans` borrows when nothing matches, and adds a `Link` span with `citation_url(key)` for known keys only, leaving the text alone. `citation_at("a [b1] c", n)` is `Some((2, 6))` for `n` in `3..=6` and `None` at 2 and 7. `suggest_key` gives `knuth1981` from `D. E. Knuth` and `1981`, `knuth1981a` if that's taken, drops characters a key can't hold (`Müller` gives `mller2020`), and returns `None` when no letter is left. `check` refuses `volume` on a `book`, `doi` on a `web`, and an empty `title`, and refuses any `kind` but `freeform` having `text`.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p kame-core references::`
Expected: fails to compile, the module has no functions yet.

- [ ] **Step 3: Implement** the interfaces above. `block_citation_ranges` uses `BlockKind::is_verbatim` and excludes `BlockKind::Image`.

- [ ] **Step 4: Run the tests and the gate**

Run: `cargo test -p kame-core references::`
Expected: all pass, then the full gate passes.

- [ ] **Step 5: Commit** `feat: find citations in a document's text`

### Task 2: IEEE entries

**Files:**
- Create: `crates/kame-core/src/references/ieee.rs`

**Interfaces:**
- Consumes: `Reference`, `Kind` from Task 1.
- Produces: `pub enum RunStyle { Plain, Italic, Url }`, `pub struct Run { pub text: String, pub style: RunStyle }`, `pub fn entry(reference: &Reference) -> Vec<Run>`, and `pub fn plain(reference: &Reference) -> String` (the runs joined), re-exported from `references`.

- [ ] **Step 1: Write the failing tests**: one for each example in the spec's "IEEE format" section, comparing `plain(...)` byte for byte with curly quotes. Build each `Reference` from the fields it needs. The `web` example is `J. Smith and J. Doe. “Obama inaugurated as President.” CNN.com. Accessed: Feb. 1, 2009. [Online]. Available: http://www.cnn.com/POLITICS/01/21/obama_inaugurated/index.html`.

Also: the Knuth article entry from the spec; seven names give `A. One et al., ...` with `et al.` in an italic run; an entry with only a title is `“Title.”` for an article; no entry holds `, ,`, `..` or a trailing space when any optional field is empty (a loop blanking each field in turn); an entry ending in a URL has no final period and any other ends in one; a DOI and accessed date come before a URL, and the URL is the last run with `RunStyle::Url`; a `freeform` entry is one plain run of exactly its `text`.

Three things the examples require: a `book` title with no edition takes a period inside the italics (`Robot Vision.`); editors in a chapter are `J. S. Brake, Ed.` and two or more are `..., Eds.`; and a book with only editors reads `X. Editor, Ed., *Title.*`.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p kame-core references::ieee`
Expected: fails to compile.

- [ ] **Step 3: Implement** `entry`, building runs in the spec's order. Keep field text exactly as typed: no abbreviating and no case changes.

- [ ] **Step 4: Run the tests and the gate**. Expected: pass.

- [ ] **Step 5: Commit** `feat: write a reference out in IEEE format`

### Task 3: The References section in exports

**Files:**
- Modify: `crates/kame-core/src/export/html.rs`, `crates/kame-core/src/export/markdown.rs`, `crates/kame-core/src/export/mod.rs`, `crates/kame-core/src/session.rs`
- Modify callers so the workspace builds: `src/main.rs:2527-2550`, `crates/kame-site/src/plan.rs:251`, `crates/kame-site/src/feed.rs:63`

**Interfaces:**
- Consumes: Tasks 1 and 2.
- Produces:
  - `Publish` gains `pub references: &'a References`; `Publish::plain()` uses an empty static.
  - `pub fn to_html_with(doc, metadata, references: &References) -> String`; `to_html(doc)` passes none.
  - `pub fn to_markdown_with(doc, metadata, references: &References) -> String`; `to_markdown(doc)` is unchanged and lists nothing.
  - `Session::export_markdown_to(&mut self, path, references: &References)` and `export_html_to` likewise. `export_markdown()` and `export_html()` keep no argument and pass none.
  - Task 6 fills the site's `Publish`; until then it passes an empty list. The app passes an empty list until Task 7.

- [ ] **Step 1: Write the failing tests** in `export/html.rs` and `export/markdown.rs`, with a helper making a document and a `References` holding `knuth1981`:

```rust
#[test] fn a_document_citing_nothing_exports_as_it_always_did() {
    // Same bytes with and without references, in both formats, including a known key's absence.
}
#[test] fn a_citation_links_to_its_entry_at_the_very_end() {
    let html = html_of("Broken [knuth1981].", &refs);
    assert!(html.contains("Broken <a href=\"#ref-knuth1981\">[knuth1981]</a>."));
    assert!(html.contains("<section class=\"references\">\n<h2>References</h2>\n<p id=\"ref-knuth1981\">[knuth1981] D. E. Knuth"));
    assert!(html.find("class=\"footnotes\"") < html.find("class=\"references\""));
}
#[test] fn markdown_keeps_a_citation_as_text_and_lists_it_after_the_footnotes() {
    let md = md_of("Broken [knuth1981].", &refs);
    assert!(md.contains("Broken \\[knuth1981\\]."));
    assert!(md.contains("\n## References\n\n\\[knuth1981\\] D. E. Knuth"));
}
```

Also: a key cited only in a footnote is listed in both formats and its footnote links in HTML; a key cited only in a marginal is listed and linked in HTML and not listed in Markdown; a key only in a note, in code, or inside a link's words isn't listed; an unknown key stays as typed with no section; a citation in bold and across a `<mark>` nests validly; two runs give the same bytes; a title of `<script>alert(1)</script>` is escaped; a `javascript:alert(1)` URL is plain text in HTML and escaped text in Markdown, while `https://a.b/c_d` is `<a href>` in HTML and `<https://a.b/c_d>` in Markdown; italic runs are `<em>` and `*...*`; a freeform text with newlines stays one Markdown paragraph and escapes `*`; `to_markdown_with` scans the document after `without_repeated_title`. Add a `Session` test that `export_html_to` writes the section.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p kame-core export::`
Expected: fails to compile on the new arguments, then on the assertions.

- [ ] **Step 3: Implement.** In `render_inline`, treat known `block_citation_ranges` like the `links` ranges, wanting `InlineTag::Link { url: citation_url(key) }`. Footnote and marginal bodies get a helper that escapes the text and wraps known citations in an anchor. Markdown body text needs no change. Add a `.references` rule to `STYLE` (hanging indent, as `ol.footnotes` is styled); sites get it through `kame_css`.

- [ ] **Step 4: Run the tests and the gate**. Existing tests must stay green, with call sites updated to pass an empty list. Expected: pass.

- [ ] **Step 5: Commit** `feat: end exports with the references they cite`

### Task 4: The list in a notebook

**Files:**
- Create: `crates/kame-notebook/src/references.rs`
- Modify: `crates/kame-notebook/src/lib.rs`, `crates/kame-notebook/src/list.rs:65` (`walk`), `docs/spec/notebook.md` (sections 1, 2 and 7.1)

**Interfaces:**
- Consumes: `Reference`, `References`, `valid_key`, `cited`.
- Produces, in `kame_notebook::references`:
  - `pub const FOLDER: &str = "references";`
  - `pub struct Loaded { pub references: References, pub problems: Vec<String> }`
  - `pub fn load(notebook: &Notebook) -> Loaded`
  - `pub fn save(notebook: &Notebook, key: &str, reference: &Reference) -> Result<(), String>`
  - `pub fn remove(notebook: &Notebook, key: &str) -> Result<(), String>`
  - `pub fn citing(notebook: &Notebook) -> BTreeMap<String, Vec<PathBuf>>`: reads every document, and counts marginals.
  - `pub fn stamp(notebook: &Notebook) -> Vec<(String, SystemTime, u64)>`: names, times and lengths, so the app can tell the list changed without parsing it.

- [ ] **Step 1: Write the failing tests** in the module, using `kame_test_dir` folders:

```rust
#[test] fn a_reference_comes_back_as_it_was_saved() { /* title "Softw.: Pract. Exper." and multi-line freeform text */ }
#[test] fn the_key_is_the_file_name_and_never_in_the_file() { /* no "key" in the yaml; no empty fields written */ }
#[test] fn a_bare_number_reads_as_text() { /* year: 1981, volume: 11 */ }
#[test] fn a_damaged_file_is_reported_and_the_others_load() {
    // "kind: nonsense", "kind: article\ntitle: x\nbogus: 1", and a volume on a book each give a problem naming the file.
}
#[test] fn a_damaged_file_is_never_overwritten() { /* save to that key is Err and its bytes are unchanged */ }
#[test] fn keys_differing_only_in_case_cant_be_saved() {}
#[test] fn two_files_differing_only_in_case_use_the_first_and_report_the_other() {}
#[test] fn what_isnt_a_reference_is_left_out() {
    // hidden files, a ".sync-conflict-" copy, "x.building", a symlink, a non-yaml file, a badly named "1.yaml" (reported).
}
#[test] fn an_interrupted_save_leaves_the_old_file() { /* temporary name starts with '.', renamed into place */ }
#[test] fn citing_finds_each_document_once_counting_footnotes_and_marginals_but_not_notes() {}
```

In `list.rs`'s tests: a `.yaml` directly in root `references/` isn't an entry; `references/essay.kame`, `references/deep/x.yaml` and `references/paper.pdf` still are; a `sub/references/x.yaml` still is. A notebook with no `references/` folder loads empty with no problems.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p kame-notebook references`
Expected: fails to compile.

- [ ] **Step 3: Implement**, with `yaml_serde`, calling `Reference::check` after parsing. Update `docs/spec/notebook.md` with the rules in the spec's "The list" section, including the fields table.

- [ ] **Step 4: Run the tests and the gate**. Expected: pass.

- [ ] **Step 5: Commit** `feat: keep a notebook's references in a folder`

### Task 5: Renaming a key

**Files:**
- Modify: `crates/kame-core/src/session.rs`, `crates/kame-notebook/src/references.rs`

**Interfaces:**
- Consumes: `block_citation_ranges`, `key_of`, `rewrite_citation`, `Session::replace_text`, `Session::set_annotation_body`, `save`, `remove`.
- Produces:
  - `Session::rename_citation(&mut self, old: &str, new: &str) -> usize`: the number of citations changed, across blocks (working from the last range to the first), footnotes and marginals. Notes are untouched. The caret stays on its text.
  - `kame_notebook::references::rename(notebook: &Notebook, old: &str, new: &str, open: Option<&Path>) -> Result<Renamed, String>`
  - `pub struct Renamed { pub rewritten: Vec<PathBuf>, pub failed: Vec<(PathBuf, String)> }`

`rename` refuses an invalid `new`, or one taken in any case. It writes the new file first, rewrites each citing document except `open` (the app renames that one through its own session), and removes the old file last, so an interruption leaves both keys and loses nothing. Markdown documents go in `failed` with a reason, because kame can't write them. So does any `.kame` that won't open or that opens read-only as newer.

- [ ] **Step 1: Write the failing tests**:

```rust
// session.rs
#[test] fn renaming_a_citation_leaves_everything_else_alone() {
    // "[old] [old2] é[old]" gives "[new] [old2] é[new]"; a code block, a note and a link's words stay;
    // a footnote and a marginal change; undo restores each one.
}
// kame-notebook references.rs
#[test] fn renaming_rewrites_each_document_and_moves_the_file() {}
#[test] fn renaming_skips_the_open_document_and_says_nothing_of_it() {}
#[test] fn renaming_names_a_markdown_document_it_cant_write_and_keeps_going() {}
#[test] fn renaming_to_a_taken_key_changes_nothing() {}
#[test] fn a_renamed_document_in_a_logged_folder_keeps_the_rename_after_it_syncs() {
    // .kame-logs in the notebook, rename, then kame_sync::logs::sync on the document, then read it: "[new]".
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p kame-core renaming_a_citation && cargo test -p kame-notebook renaming`
Expected: fails to compile.

- [ ] **Step 3: Implement.** Check `kame_sync::logs::sync`'s real signature in `crates/kame-sync/src/logs.rs` when writing that test, and check how a read-only session reports itself in `session.rs`.

- [ ] **Step 4: Run the tests and the gate**. Expected: pass.

- [ ] **Step 5: Commit** `feat: rename a reference's key across a notebook`

### Task 6: Sites

**Files:**
- Modify: `crates/kame-site/src/plan.rs:199-260`, `crates/kame-site/src/feed.rs:36-70`, `crates/kame-site/src/notebook.rs:303-345`

**Interfaces:**
- Consumes: `references::load`, `unresolved`, `Publish::references`.
- Produces: `plan_site(site_dir, config, sources, references: &References, problems)` and `feed::render(config, sources, index, site_dir, references: &References)`. `build_plan` (a `site.yaml` site) passes an empty list, and `plan_notebook` passes the notebook's.

- [ ] **Step 1: Write the failing tests** in `notebook.rs`'s test module:

```rust
#[test] fn a_published_document_lists_what_it_cites_and_nothing_else() {
    // Two references, one cited. The page has the entry and the link. "Uncited Title" is nowhere in output/.
}
#[test] fn a_reference_cited_only_by_an_unpublished_document_reaches_nothing() {}
#[test] fn no_reference_file_reaches_the_site() { /* no .yaml from references/ in the plan's files */ }
#[test] fn a_lost_key_warns_and_stays_as_typed() {
    // "[smith2020]" warns, naming the document and key, and is in the page. "[sic]" doesn't warn.
}
#[test] fn a_damaged_reference_is_a_warning_not_a_failed_build() {}
#[test] fn the_feed_lists_references_too() {}
```

And in `plan.rs`'s tests, `a_site_yaml_site_leaves_a_key_as_text`.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p kame-site references`
Expected: fails to compile, or fails on the assertions.

- [ ] **Step 3: Implement.** Warnings use `problems.warnings`.

- [ ] **Step 4: Run the tests and the gate**. Expected: pass.

- [ ] **Step 5: Commit** `feat: list references on a notebook's site`

### Task 7: Known keys on the page

**Files:**
- Create: `src/references.rs` (the app's side, as `src/publish.rs` is for the Site menu)
- Modify: `crates/kame-check/src/lib.rs:156-180`, `src/main.rs` (`layout_of` at 5423, where the page builds its blocks, `url_at` callers, `Command::Insert` at 3162, `run_file_command` at 2527), `src/notebook.rs` (`notebook_tick`, at 178)

**Interfaces:**
- Consumes: `with_citation_spans`, `cited_key`, `citation_at`, `valid_key`, `references::load`, `stamp`, `unresolved`, `ieee::plain`.
- Produces, in `src/references.rs`:
  - `pub(crate) struct ReferencesUi { pub(crate) list: References, problems: Vec<String>, stamp: Vec<...>, pub(crate) shown: Option<String> }`, as `KameApp.refs`.
  - `pub(crate) fn just_closed_note(block: &Block, caret: usize, references: &References) -> Option<String>`
  - `pub(crate) fn unresolved_note(doc: &Document, references: &References, marginals: bool) -> Option<String>`

**How known keys draw:** the page and `layout_of` both shape `with_citation_spans(block, &self.refs.list)` rather than the block itself. The copy is what's shaped and hit-tested, never edited, so `LINK_COLOR`, no-hyphenation and `url_at` all work unchanged, and the layout cache's fingerprint sees the new spans when references change. Both places must use it, or caret movement and painting can disagree on line breaks. A click whose `url_at` gives a `cited_key` sets `refs.shown` and doesn't follow the link. A real link can't collide, because `link_style` turns any `#...` URL into a `DocumentLink`.

**Spell checking:** `check_block` skips every `citation_ranges` range in non-verbatim blocks, known or not, so the background checker needs no list. This goes slightly past the spec, in that a misspelled `[teh]` isn't marked.

- [ ] **Step 1: Write the failing tests**: in `kame-check`, `a_citation_isnt_a_misspelling` (`[zzqxj2020]` gives no finding, and `zzqxj` outside brackets still does). In `src/references.rs`:

```rust
#[test] fn closing_an_unknown_key_says_how_to_add_it() {
    // Caret right after "[kleppmann2019]" gives exactly "No reference called kleppmann2019. Ctrl-Shift-C adds it."
    // A known key, "[a b]", a caret elsewhere, and a code block give None.
}
#[test] fn an_export_with_a_lost_key_says_so() {
    // unresolved_note names smith2020, and is None for [sic].
}
```

In `src/main.rs`'s tests: `a_citation_shapes_the_same_for_the_caret_and_the_page`, comparing a line break through `layout_of` with the shaped copy.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --bin kame references && cargo test -p kame-check a_citation`
Expected: fails to compile.

- [ ] **Step 3: Implement.** In `notebook_tick`, reload when `stamp` changes or the notebook changes, and show load problems the way that function reports notebook errors. Outside a notebook the list is empty. The Insert note fires only when the typed text was `]`, and sets `self.notice`. Export passes `&self.refs.list`, appending `unresolved_note` to its notice. `refs.shown` draws a small window with `ieee::plain` and an Edit button (Task 8 wires the button), closing on Escape or clicking elsewhere, and counts in `dialog_open`.

- [ ] **Step 4: Run the tests and the gate**. Expected: pass.

- [ ] **Step 5: Commit** `feat: draw known citations on the page`

### Task 8: Entering and managing references

**Files:**
- Modify: `src/references.rs`, `src/main.rs` (the `Ctrl-K` chord at 3014-3031, the Annotate menu, the context menu near `LinkMenu`, `dialog_open` at 2490, the prompt dispatch at 5174)

**Interfaces:**
- Consumes: Tasks 1, 2, 4 and 5, and `ReferencesUi` from Task 7.
- Produces, pure and tested, in `src/references.rs`:
  - `pub(crate) enum CiteStart { NeedsNotebook, New { key: String }, Edit { key: String }, Search }`
  - `pub(crate) fn cite_start(text: &str, offset: usize, references: &References, in_notebook: bool) -> CiteStart`
  - `pub(crate) fn search<'a>(references: &'a References, query: &str) -> Vec<&'a str>`: keys, authors and titles, ignoring case, in `sorted()` order.
  - `pub(crate) struct ReferenceForm`, with `tab: Tab`, `key`, and `reference: Reference`, plus `authors_text` and `editors_text` (comma-separated) and `key_edited: bool`.
  - `ReferenceForm::{new(key: Option<String>), editing(key, &Reference), built(&self) -> Reference, key_problem(&self, &References) -> Option<String>, can_save(&self, &References) -> bool, preview(&self) -> String, suggest(&mut self, &References)}`

`pub(crate) enum Tab { Structured, Freeform }`. Saving on the Freeform tab sets `kind` to `Freeform` and clears nothing, except that `built()` keeps only the fields that kind has, so `check` passes.

- [ ] **Step 1: Write the failing tests**:

```rust
#[test] fn cite_opens_what_the_caret_is_on() {
    // Outside a notebook: NeedsNotebook. In or after "[new1]": New. In or after a known key: Edit.
    // Anywhere else, and in "[a b]": Search.
}
#[test] fn search_looks_through_keys_authors_and_titles() {}
#[test] fn names_split_on_commas_and_keep_their_spelling() {}
#[test] fn a_suggested_key_follows_the_fields_until_its_edited() {}
#[test] fn the_form_refuses_a_bad_key_a_taken_key_and_no_title() {
    // A taken key is allowed when it's the key being edited.
}
#[test] fn freeform_saves_only_a_key_and_text_and_shows_them_as_typed() {}
#[test] fn what_the_form_builds_always_passes_check() { /* each kind */ }
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --bin kame references`
Expected: fails to compile.

- [ ] **Step 3: Implement the logic, then the windows.** Follow `show_link_prompt` for how a prompt is shown, focused, and closed with Enter and Escape.

- **Ctrl-Shift-C** (`modifiers.command && modifiers.shift`, key `C`) and Annotate > Cite…, plus a Cite… row in the right-click menu, call `begin_cite`, which acts on `cite_start`. `NeedsNotebook` sets the notice `Citing needs a notebook.`
- **Search window:** a text box and rows. Up, Down and Enter choose, and choosing does `session.insert("[key]")`. The last row is `New reference “query”…`, opening the form with the query as the key if `valid_key`.
- **New reference and edit window:** the two tabs, the kind selector, fields for that kind, the live preview, and Add and cite / Save / Cancel. Add and cite inserts `[key]` unless the caret is already on that key. A save failure stays in the window as text.
- **References… window** (Annotate > References…, notebook only): the sorted list with citing counts from `citing` on a background thread, like `start_backlinks`, plus any `problems`. Edit opens the form. Delete and rename take a confirmation row saying how many documents cite the key. Rename calls `references::rename` with the open path and `session.rename_citation` for the open document, and lists `failed` in the window.
- The Edit button from Task 7's window opens the form.

- [ ] **Step 4: Run the tests and the gate**. Expected: pass.

- [ ] **Step 5: Commit** `feat: enter and manage references from the app`

### Task 9: Docs and the real app

**Files:**
- Modify: `docs/guide.md`, `docs/keybindings.md`, `AGENTS.md` (module map), `docs/followups.md` if anything is left over

- [ ] **Step 1: Write the docs.** A References section in the guide covering citing, the two tabs, the windows, the export section, and the limits (notebook only, footnotes plain, which lost keys warn). Add Ctrl-Shift-C to the keybindings table.

- [ ] **Step 2: Drive the real app** with a scratch notebook in the scratchpad, by the method in the repo's history: `osascript` keystrokes and `screencapture -l WINDOWID`. Check that kame is frontmost before sending keys. See each of these on screen and keep the screenshots:
  1. Type `[kleppmann2019]` and see the status note, with plain drawing.
  2. Ctrl-Shift-C, fill the form, save. The text is now in the link colour.
  3. Ctrl-Shift-C elsewhere, search, choose. `[key]` goes in, and undo takes it out.
  4. Freeform tab: add one, and cite it.
  5. Export HTML and Markdown and read the files.
  6. Site > Build and read `output/`.
  7. References…: rename and delete, with a second document citing the key.

`click at` is denied here, so anything needing a pointer (the click on a citation, right-click menus) can't be driven. Say so in the report and leave those for the user to try.

- [ ] **Step 3: Run the gate and `make all`.** Expected: exit 0 for each.

- [ ] **Step 4: Commit** `docs: describe references`
