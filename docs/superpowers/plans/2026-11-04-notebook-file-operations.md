# Notebook File Operations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Right-clicking a document in the notebook browser renames, moves or deletes it, with links rewritten, and the result holds on other machines.

**Architecture:** `kame-core` learns to rewrite a document's links and to say a moved target. `kame-sync` learns when a database no longer belongs to the logs beside it, and stops writing for it. `kame-notebook` gets two new modules: `manage` (checks, delete, plan, move) and `stranded` (the clean-up). `src/notebook.rs` adds the menu, dialogs and report, and owns everything about the open document.

**Tech Stack:** Rust workspace, egui, SQLite via `kame_core::storage::Db`, the logs in `kame_sync::revlog`.

**Spec:** `docs/superpowers/specs/2026-11-04-notebook-file-operations-design.md`. Read it first. It gives the reasons, and this plan doesn't repeat them.

## Global Constraints

- All text offsets are byte offsets. Every text change goes through an `Op`.
- No new runtime dependency, including in `kame-notebook` (so no `uuid` there).
- Logic goes in the libraries. `src/` holds only the window's part. The layering is one-way: core knows nothing of the others.
- Tests are headless, and their files go in `kame_test_dir::folder(name)`, never directly in the temporary directory. Don't touch `~/data`.
- A stranded database is only ever renamed, never deleted, and never moved until seen stranded on a pass at least 10 minutes after first being seen.
- A retired database is named `.X.retired.kame`, numbered if taken (`.X.retired-2.kame`).
- Refusal for a taken path says to delete the other file first: `something is already at {relative}; delete it first`.
- Delete's question reads: `Delete {title}? This can't be undone.`
- The stranded notice reads: `this document was moved or deleted on another machine, and isn't syncing`.
- Prose in comments and docs follows the repo's plain style. `make all` and `cargo test` must pass at the end of every task.
- Commit only the files the task changed, and end each commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Names with spaces or non-ASCII letters** (`maritain/matière première.kame`): links resolve and the dialog selects the right characters, since egui's cursor counts characters and our offsets are bytes.
2. **Odd typed paths** (empty, `..`, `/abs`, `~/x`, `./a.kame`, a trailing `/`, surrounding spaces, or through a symbolic link leading out): tidied or refused, with nothing changed.
3. **Renaming or deleting the root `Notebook.kame`:** links to it still lead somewhere after a rename, and Home says the notebook has none after a delete, with no panic.
4. **A linking document that can't be written** (a read-only file): it's listed in the report and the others still change.
5. **A document with unsaved typing at the moment it moves:** every keystroke is on disk already through `record`, so all of it is there after the move, along with caret and scroll.

---

### Task 1: Rewriting links and titles in `kame-core`

**Files:**
- Modify: `crates/kame-core/src/links.rs`
- Modify: `crates/kame-core/src/session.rs`
- Modify: `crates/kame-core/src/storage/db.rs`

**Interfaces:**
- Produces:
  - `kame_core::links::moved_target(target: &str, new_file: &Path, document_after: &Path) -> String`
  - `kame_core::links::own_target_after_move(target: &str, from: &Path, to: &Path) -> String`
  - `Session::rewrite_links(&mut self, rewrite: impl FnMut(&str) -> Option<String>) -> Result<usize, String>`
  - `Session::follow_rename(&mut self, old_stem: &str, new_stem: &str) -> bool`
  - `Db::clear_sync_state(&mut self) -> Result<(), DbError>`

- [ ] **Step 1: Write the failing tests** in `links.rs`'s test module, on real folders from `kame_test_dir`, since `moved_target` canonicalizes:

```rust
#[test]
fn a_moved_target_keeps_its_form() {
    let root = kame_test_dir::folder("moved-target").canonicalize().unwrap();
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    let doc = root.join("a/doc.kame");
    let new = root.join("b/new.kame");
    assert_eq!(moved_target("old.kame", &new, &doc), "../b/new.kame");
    assert_eq!(moved_target("old", &new, &doc), "../b/new.kame");
    assert_eq!(moved_target("old.kame#Part two", &new, &doc), "../b/new.kame#Part two");
    let absolute = new.display().to_string();
    assert_eq!(moved_target(root.join("a/old.kame").to_str().unwrap(), &new, &doc), absolute);
}

#[test]
fn a_tilde_target_stays_under_the_home_folder() {
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let new = home.join("notes/new.kame");
    assert_eq!(moved_target("~/notes/old.kame", &new, Path::new("/x/doc.kame")), "~/notes/new.kame");
}

#[test]
fn a_folder_link_stays_a_folder_link_only_while_it_is_the_index() {
    let root = kame_test_dir::folder("moved-index").canonicalize().unwrap();
    std::fs::create_dir_all(root.join("a")).unwrap();
    let doc = root.join("doc.kame");
    assert_eq!(moved_target("a/", &root.join("a/Notebook.kame"), &doc), "a/");
    assert_eq!(moved_target("a/", &root.join("a/other.kame"), &doc), "a/other.kame");
}

#[test]
fn a_moved_documents_heading_link_still_means_itself() {
    let root = kame_test_dir::folder("own-target").canonicalize().unwrap();
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    let (from, to) = (root.join("a/doc.kame"), root.join("a/b/doc.kame"));
    assert_eq!(own_target_after_move("#Part two", &from, &to), "#Part two");
    assert_eq!(own_target_after_move("other.kame", &from, &to), "../other.kame");
    assert_eq!(own_target_after_move("https://example.com", &from, &to), "https://example.com");
}
```

In `session.rs`'s tests, using the helpers already there:

- `rewriting_links_changes_both_kinds_and_an_images_link_as_one_undo_step`: a document with a `DocumentLink`, a `Link` and an image block with `link: Some(..)`. `rewrite_links` mapping all three returns `Ok(3)`, `history()` grows by one, and one `undo()` restores all three. Block text and annotations are unchanged.
- `rewriting_no_links_writes_nothing`: a closure returning `None` gives `Ok(0)` and `history()` doesn't change.
- `rewriting_links_refuses_a_document_from_a_newer_version`: after `mark_newer()`, `Err`.
- `a_rename_changes_a_title_still_matching_the_old_name`: metadata and document titles both `foo`, `follow_rename("foo", "bar")` makes both `bar` and returns `true`.
- `a_rename_leaves_a_title_the_author_set`: metadata title `On Matter`, `follow_rename("foo", "bar")` leaves it. A `Document::title` of `foo` still changes.
- `saving_as_clears_the_sync_state`: `set_sync_state("x")`, `save_as(other)`, then `sync_state()` is `None`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p kame-core`
Expected: compile errors naming the missing functions.

- [ ] **Step 3: Implement.**

`moved_target`: split with `split_document_target`, keep everything after the path as written. A path starting `~/` becomes `~/` + `new_file` relative to `$HOME`, using how `resolve_open_path` finds home. An absolute path becomes `new_file`. A path ending `/` stays as written if `new_file.file_name() == "Notebook.kame"` and `new_file.parent()` is what that path resolves to from `document_after`; otherwise it falls through to relative. Anything else is `path_between(document_after's real folder, new_file)`. `core` can't name `kame_notebook::INDEX`, so take the literal locally with a comment.

`own_target_after_move`: `target` unchanged if its path part is empty, else `retargeted(target, from, to)`.

`rewrite_links`: `Err` if `newer` or `save_failed`. Clone the document, apply `rewrite` to each `SpanStyle::Link.url`, `SpanStyle::DocumentLink.target` and `BlockKind::Image.link`, keeping the variant. Count real changes. If any, `replace_document`, returning `Err` if that fails.

`follow_rename`: changes `metadata.title` through `set_metadata` and `Document::title` through `replace_document`, each only if equal to `old_stem`. Returns whether anything changed.

`clear_sync_state`: `DELETE FROM meta WHERE key = ?1`. `save_as` calls it on the new `self.db` straight after `Db::open(path)`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p kame-core && cargo test -p kame-sync`
Expected: all pass. `kame-sync` matters because of the Save As change. If a test there relied on the copy's sync state, fix the test only if the spec's reasoning covers it, and note it in the commit.

- [ ] **Step 5: Commit** `feat: rewrite a document's links and follow a rename's title`

---

### Task 2: Knowing a database is stranded, and holding off

**Files:**
- Modify: `crates/kame-sync/src/logs.rs`
- Test: `crates/kame-sync/tests/logged.rs`

**Interfaces:**
- Consumes: `Session::save_as` clearing sync state (Task 1).
- Produces:
  - `kame_sync::logs::is_stranded(last: Option<&str>, folder: &Path) -> bool`
  - `Outcome { .., pub stranded: bool }`

- [ ] **Step 1: Write the failing tests** in `tests/logged.rs`, with its `two_machines()` helper:

- `a_database_without_a_sync_state_is_never_stranded`: `is_stranded(None, missing)` is `false`.
- `a_recorded_revision_with_no_logs_is_stranded`: `is_stranded(Some("r"), missing)` is `true`.
- `a_recorded_revision_in_the_logs_beside_it_is_not_stranded`: sync once, read `session.sync_state()`, `is_stranded` is `false`.
- `logs_that_belong_to_another_document_leave_it_stranded`: a sync state from one document against the log folder of a different one is `true`.
- `logs_that_cannot_be_read_do_not_count_as_stranded`: replace the log folder with a plain file, so that `Store::load` fails, and it's `false`. Nothing is done on a guess.
- `logs_going_while_open_stop_all_writing`: open `Logged`, sync, `remove_dir_all(log_folder)`, edit, `sync` gives `outcome.stranded == true`, and the folder still doesn't exist afterwards.
- `opening_stranded_writes_nothing`: as above but with a fresh `Session` and `Logged::open` after the folder went.
- `logs_replaced_while_open_stop_all_writing`: open `Logged`, sync, delete the folder, make a different document's logs there, edit, `sync`: `stranded` is `true` and the new logs hold none of the old words (`Store::load`, `heads`, `document`).
- `a_first_sync_is_not_stranded`: a document that has never synced still writes its history, as now.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p kame-sync --test logged`
Expected: compile error on `is_stranded`.

- [ ] **Step 3: Implement.**

`is_stranded`: `false` for `None`. `true` if `folder` isn't there. `Store::load(folder)`, `false` on `Err`, else `!store.contains(last)`.

In `Logged::write_and_merge`, first thing after the `save_failed` guard: the in-memory store keeps revisions after their files go, so ask the disk, and only when something could have changed: if `self.last` is `Some` and (the folder is gone or `self.has_news()`), call `is_stranded(self.last.as_deref(), &self.replica.folder)`, and return `Ok(Outcome { stranded: true, ..Default::default() })` if it says so. A `Store::refresh` must not have run first, since a missing folder may be created by a write. Keep `sync` from calling `mark_newer` on this path if that would misfire.

The test at `tests/logged.rs:552` ("As after Save As, or the logs being cleared") describes behaviour this replaces for logs cleared under a marker. Save As now clears the state, so keep the test by making its setup a Save As. A separate logs-cleared case belongs to `logs_going_while_open_stop_all_writing`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p kame-sync`
Expected: all pass.

- [ ] **Step 5: Commit** `feat: stop writing logs for a database they no longer belong to`

---

### Task 3: Checking a new path and deleting

**Files:**
- Create: `crates/kame-notebook/src/manage.rs`
- Modify: `crates/kame-notebook/src/lib.rs` (add `pub mod manage;`, and reword the header's "only ever reads them"), `crates/kame-notebook/src/links.rs` (`in_documents_area` becomes `pub(crate)`, and `remove_untouched`'s comment no longer says it's the only thing that deletes)
- Modify: `AGENTS.md` (the `kame-notebook` paragraph says it can rename, move and delete)
- Test: `crates/kame-notebook/tests/manage.rs`

**Interfaces:**
- Produces:
  - `manage::check_new_path(notebook: &Notebook, document: &Path, typed: &str) -> Result<PathBuf, String>`
  - `manage::delete_document(path: &Path) -> std::io::Result<()>`
  - `manage::belonging_files(path: &Path) -> Vec<PathBuf>`, the conflict, merged and retired copies named for a document

- [ ] **Step 1: Write the failing tests** in `tests/manage.rs`, with the `folder`, `document` and `notebook` helpers as in `tests/notebook.rs`:

```rust
#[test]
fn a_new_path_is_tidied_and_given_the_documents_extension() {
    let (root, nb) = /* a notebook holding a.kame and note.md */;
    let a = root.join("a.kame");
    assert_eq!(check_new_path(&nb, &a, "b").unwrap(), root.join("b.kame"));
    assert_eq!(check_new_path(&nb, &a, "  ./x/y.kame ").unwrap(), root.join("x/y.kame"));
    assert_eq!(check_new_path(&nb, &a, "x/matière première").unwrap(), root.join("x/matière première.kame"));
    assert_eq!(check_new_path(&nb, &root.join("note.md"), "n").unwrap(), root.join("n.md"));
}

#[test]
fn a_new_path_is_refused_with_nothing_changed() {
    let (root, nb) = /* as above, plus a symbolic link out, and a taken name */;
    let a = root.join("a.kame");
    for typed in ["", "   ", "../out", "/etc/x.kame", "~/x.kame", "x/", ".hidden", "x/.y.kame",
                  "static/a.kame", "images/a.kame", "theme/a.kame", "output/a.kame",
                  "a.sync-conflict-1.kame", "a.txt", "taken", "logs-only", "out-link/a.kame"] {
        assert!(check_new_path(&nb, &a, typed).is_err(), "{typed}");
    }
    assert!(!root.join("x").exists());
}

#[test]
fn the_refusal_for_a_taken_path_says_to_delete_it_first() {
    let err = check_new_path(&nb, &a, "taken").unwrap_err();
    assert_eq!(err, "something is already at taken.kame; delete it first");
}

#[test]
fn a_new_name_differing_only_in_case_is_allowed_where_the_disk_sees_one_file() {
    // Only asserts on a case-insensitive disk: skip the assertion if `A.kame` doesn't exist as `a.kame`.
}

#[test]
fn deleting_leaves_nothing_named_for_the_document() {
    // X.kame, X.kame-wal, X.kame-shm, .X.kame.sync/titan.log, X.sync-conflict-1.kame,
    // X.sync-conflict-1.kame.merged, .X.retired.kame, .X.retired-2.kame, and a neighbour XY.kame.
    delete_document(&x).unwrap();
    // All gone but XY.kame.
}

#[test]
fn deleting_a_markdown_document_removes_only_its_file() {}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p kame-notebook --test manage`
Expected: compile error.

- [ ] **Step 3: Implement.**

`check_new_path`: trim, then refuse empty, a leading `/` or `~`, and a trailing `/`. Join to the root and tidy with `links::tidied`, refusing anything no longer under the root. Add the document's extension if it has none, and refuse a different one (case-insensitive compare). Refuse `.sync-conflict-` in the name. Use `holds` and `in_documents_area`. Taken if `symlink_metadata` finds the path or `log_folder(path)` is there, unless `same_file` says it's `document`. Each refusal has its own one-line message.

`belonging_files`: `kame_sync::files::conflict_copies(path)`, each with its `.merged` sibling if there, and `.{name}.retired*.kame` for the stem, by reading the folder. Exact stem match, so `XY` isn't taken for `X`.

`delete_document`: remove the file and belonging files, ignoring not-found on `-wal` and `-shm`, then `remove_dir_all` the log folder if it's there. An error on the main file returns before the logs go.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p kame-notebook`
Expected: all pass.

- [ ] **Step 5: Commit** `feat: check a document's new path and delete a document`

---

### Task 4: Planning and carrying out a move

**Files:**
- Modify: `crates/kame-notebook/src/manage.rs`
- Test: `crates/kame-notebook/tests/manage.rs`

**Interfaces:**
- Consumes: Task 1's functions, `manage::belonging_files`, `Notebook::follow`, `kame_sync::logs::{Logged, build_from_logs, logging_root, log_folder}`.
- Produces:

```rust
pub struct Left { pub path: PathBuf, pub relative: String, pub why: String }
pub struct MovePlan {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Each document with links to change: its old target string to its new one.
    rewrites: Vec<(PathBuf, HashMap<String, String>)>,
    left: Vec<Left>,
}
#[derive(Debug, Default)]
pub struct MoveReport { pub changed: Vec<String>, pub left: Vec<Left> }

pub fn plan_move(notebook: &Notebook, from: &Path, to: &Path,
                 open: Option<(&Path, &Document)>, device: &str, now: i64) -> MovePlan;
pub fn carry_out(notebook: &Notebook, plan: MovePlan, open: Option<&mut Session>,
                 device: &str, now: i64) -> Result<MoveReport, String>;
```

`from` must not be the path of `open`'s session in `carry_out`: the app leaves a moving document first, so it's handled like any closed one. `open` in `plan_move` may still be the moving document, since it's only read.

- [ ] **Step 1: Write the failing tests.** Each builds a real notebook with `document()` plus a helper `link(path, words, SpanStyle)`, and proves a link by `notebook.follow(target, base) == Follow::Open(new)`:

- `every_form_of_link_follows_a_rename`: `DocumentLink` with `a.kame`, with `a`, with `a.kame#Heading` (heading kept), `Link` with `a`, an image `link`, an absolute path, and a link from a subfolder as `../a.kame`.
- `a_link_elsewhere_is_left_alone`: same-named file in another folder, web link, `#Heading`.
- `a_move_across_folders_fixes_its_own_links_and_images`: `own` links to `sib.kame` and `#Heading`, and shows `images/p.png`. Move to `deep/er/own.kame`: the sibling link becomes `../../sib.kame`, the heading link is unchanged, and the image path leads to the same file.
- `a_document_linking_to_itself_by_name_follows`: `a.kame` links to `a.kame`, rename to `b`: it's `b.kame`.
- `a_rename_carries_files_and_history`: after moving, `-wal`, `.sync` folder and conflict copies are at the new name, nothing's at the old, and `Session::open(to)`'s `history()` and `snapshots()` match.
- `a_rename_changes_a_matching_title_and_a_move_within_the_same_name_does_not`.
- `a_title_change_reaches_the_logs`: in a logged folder, `Store::load(log_folder(to))`'s head `metadata.title` is the new name.
- `changes_to_a_closed_logged_document_reach_its_logs`: the linking document's head in its store has the new target.
- `a_document_with_only_logs_is_changed_too`: remove a linker's database first, leaving logs. After the move, its head has the new target.
- `a_link_only_in_the_logs_is_found`: a second device's log adds a link to a closed document's logs, with its database behind (as `tests/logged.rs` plays it, by copying log files, here with `kame_sync` directly). After the move, the head has the new target and no old one.
- `markdown_and_newer_documents_are_listed_and_untouched`: `.md` bytes identical, and the `left` entries say why. A moved `.md` with relative links across folders is listed too.
- `a_document_that_cannot_be_written_is_listed_and_the_others_still_change`: a linker chmod 0444. (Review Focus 4.)
- `the_open_document_changes_through_its_session`: pass `Some(&mut session)` for a linker. `session.document()` has the new target, and it's in `history()`.
- `renaming_the_root_index_keeps_links_to_it`: `Notebook.kame` to `Home.kame`. A `./`-style folder link and a named one both follow to `Home.kame`, `notebook.index()` is `None`. (Review Focus 3.)
- `a_move_with_unsaved_typing_keeps_all_of_it`: type through a `Session`, drop it, `plan_move` and `carry_out`, reopen: every word is there. (Review Focus 5.)
- `a_name_with_spaces_and_accents_survives`: `matière première.kame` and links to it. (Review Focus 1.)
- `a_failed_move_leaves_everything_as_it_was`: make the target's folder unwritable (chmod 0555) so the rename fails. `carry_out` is `Err`, the files are at `from`, and no document changed.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p kame-notebook --test manage`
Expected: compile errors on `plan_move`.

- [ ] **Step 3: Implement.**

`plan_move`: for each of `notebook.documents()` (including `from`), find its links, kept as a private `links_in(&Document) -> Vec<&str>` over spans and image `link`, whose `follow` from that document gives `Open` of `from` by `same_file`. A logged document is also read from every `Store::heads()`. If a head has such a link that its database lacks, open a `Session` on it, sync with `Logged`, and read again. For each link, new target = `moved_target(old, to, base_after)`, where `base_after` is `to` when the document is `from`, and itself otherwise. `.md` and a document whose `Session` can't take edits go in `left`, the latter found at carrying out.

`moved_target` needs `to`'s folder to exist, so create it first (`create_dir_all`). Created folders are kept even if a later step fails, since they're invisible.

`carry_out`, in this order:
1. Rename main file, then `-wal`, `-shm`, log folder, then belonging files with the stem swapped. Undo what's renamed if any fails, and return `Err`.
2. For each other linking document: use `open` if it's that document, else `edit_closed`: `build_from_logs`, `Session::open`, refuse if `newer`, `rewrite_links` looking up the plan's map, `Logged::open` plus `sync` when `logging_root` is `Some`. Failure goes in `left` and moves on.
3. For the moved document, unless Markdown: one `edit_closed` with a closure trying the map first, then `own_target_after_move`, then `rehome_images(Some(from), Some(&notebook.root))`, then `follow_rename` if the stem changed, then sync.

`.md` moved across folders with relative links goes in `left`, with `why` saying its links weren't changed.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p kame-notebook`
Expected: all pass.

- [ ] **Step 5: Commit** `feat: move a document in a notebook, rewriting the links to it`

---

### Task 5: Cleaning up stranded databases

**Files:**
- Create: `crates/kame-notebook/src/stranded.rs`
- Modify: `crates/kame-notebook/src/lib.rs`
- Test: `crates/kame-notebook/tests/stranded.rs`

**Interfaces:**
- Consumes: `kame_sync::logs::is_stranded` (Task 2).
- Produces:

```rust
#[derive(Clone, Debug, PartialEq)]
pub enum Done {
    Moved { from: PathBuf, to: PathBuf },
    /// `opened_there` if the moved document already had a database, and it isn't lost words.
    Retired { from: PathBuf, to: PathBuf, opened_there: bool },
    Returned { from: PathBuf, to: PathBuf },
}
pub fn find_stranded(notebook: &Notebook) -> Vec<PathBuf>;
pub fn reconcile(notebook: &Notebook, may_act_on: &[PathBuf], open: Option<&Path>) -> Vec<Done>;
```

`reconcile` has no clock. The app passes only what has waited long enough.

- [ ] **Step 1: Write the failing tests.** Two machines are two notebook folders with `.kame-logs`, and the sync tool is played by copying or removing `.sync` folders, as `kame-sync/tests/logged.rs` does:

- `a_database_that_never_synced_is_not_stranded`, `one_outside_a_marked_folder_is_not_stranded`, `one_with_its_logs_is_not_stranded`.
- `a_moved_document_joins_its_logs_with_history_intact`: Machine B has `a.kame` synced. Machine A's move is played by renaming B's `.a.kame.sync` to `.b.kame.sync`. `reconcile` returns `Moved`, `b.kame`'s `history()` and snapshots match B's old ones, and the next `Logged::sync` writes nothing unexpected and leaves no `.a.kame.sync`.
- `unsynced_words_merge_after_joining`: B types before the logs move, then a `Logged` sync at the new path has those words.
- `a_deleted_document_is_retired_and_stays_gone`: remove B's log folder. `reconcile` returns `Retired`, with `.a.retired.kame` there and `a.kame` not listed. No log folder comes back.
- `a_second_retirement_gets_a_number`.
- `a_reused_path_retires_the_old_database_and_keeps_the_new_logs_clean`: B's `.a.kame.sync` replaced with another document's logs. `a.kame` is retired, the new logs hold none of the old words, and `a.kame` is then built from the logs.
- `a_swap_moves_both_through_temporary_names`: B has `a` and `b`, with the log folders swapped. Each ends with its own history at the other's name, and no temporary name is left.
- `when_the_moved_document_is_already_open_there_the_old_one_retires`: `b.kame` built from logs at the new home. `Retired { opened_there: true }`.
- `two_homes_leave_it_alone`: the same log folder copied to two names, so `reconcile` returns nothing and the database is untouched.
- `late_logs_bring_a_retired_database_back`: retire it, then add `.b.kame.sync` holding its revision. `Returned`, `b.kame` has the history.
- `a_save_as_copy_is_never_mistaken`: Save As `a` to `c`, and close without syncing. `find_stranded` doesn't include `c`, and with `a`'s logs deleted, `a` retires and `c` is untouched.
- `only_what_may_be_acted_on_is_touched`: an empty `may_act_on` does nothing while `find_stranded` still reports it.
- `the_open_document_is_never_touched`: `open` equal to a stranded path does nothing.
- `retired_files_are_not_listed_and_their_side_files_go_with_them`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p kame-notebook --test stranded`
Expected: compile error.

- [ ] **Step 3: Implement.**

`find_stranded`: for each `.kame` entry of `notebook.documents()` where `path.is_file()` and `logging_root(path).is_some()`, `Db::open_read_only`, `load_sync_state`, `is_stranded(state, &log_folder(path))`.

`reconcile`: add a private `log_folders(notebook)` that walks as `Notebook::walk` does, returning each `.X.kame.sync` with the document path it stands for, and loading each `Store` once. For each path in `may_act_on` except `open`: recheck `find_stranded`'s test, since time passed. Then the *new home* is any log folder other than its own whose store `contains` its recorded revision. Two or more means leave it alone. None means retire. One with a database that isn't itself stranded means retire with `opened_there`. One otherwise means move.

Do moves in two rounds so swaps work: each mover goes first to `.{stem}.moving-{n}.kame`, with `-wal` and `-shm`, then to its home, so a name freed by one is free for another. Anything left in a temporary name by a crash ends in `.kame` and is ignored by the sync tool and the listing, and the next pass treats a file with that name as retired. Then walk `.X.retired*.kame` beside log folders for `Returned`: logs at any path without a database that contain its recorded revision.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p kame-notebook && cargo test -p kame-sync`
Expected: all pass.

- [ ] **Step 5: Commit** `feat: retire or rejoin a database its logs have left`

---

### Task 6: The app's clean-up pass

Before the menu, so that nothing the menu can do is unsafe on another machine.

**Files:**
- Modify: `src/notebook.rs`, `src/main.rs` (`sync_logs`, `adopt_session`, `KameApp` fields)
- Modify: `docs/superpowers/specs/2026-11-03-notebook-design.md` (sections 8, 10 and 13), `docs/followups.md` (remove the Move item, and the stale-copy sentence with it)

**Interfaces:**
- Consumes: `stranded::{find_stranded, reconcile, Done}`, `Outcome::stranded`.
- Produces: `stranded_ready(seen: &mut HashMap<PathBuf, Instant>, found: &[PathBuf], now: Instant) -> Vec<PathBuf>` in `src/notebook.rs`, and `const STRANDED_WAIT: Duration = Duration::from_secs(600)`, `const STRANDED_RECHECK: Duration = Duration::from_secs(60)`.

- [ ] **Step 1: Write the failing tests** in `src/notebook.rs`'s test module:

```rust
#[test]
fn a_stranded_database_waits_ten_minutes_from_when_it_was_first_seen() {
    let start = Instant::now();
    let path = PathBuf::from("/n/a.kame");
    let mut seen = HashMap::new();
    assert!(stranded_ready(&mut seen, &[path.clone()], start).is_empty());
    assert!(stranded_ready(&mut seen, &[path.clone()], start + Duration::from_secs(599)).is_empty());
    assert_eq!(stranded_ready(&mut seen, &[path.clone()], start + Duration::from_secs(600)), [path.clone()]);
}

#[test]
fn a_database_that_stops_being_stranded_starts_again() {
    // Seen, then a pass without it, then seen again 600s later: not ready.
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --bin kame stranded`
Expected: compile error. (If the binary's test target is named differently, see `cargo test --bins`.)

- [ ] **Step 3: Implement.**

`stranded_ready` drops entries not in `found`, and adds new ones at `now`.

In `NotebookUi`, add `stranded_seen`, `stranded_checked: Option<Instant>`, and `stranded_coming: Option<Receiver<(Vec<Done>, Vec<PathBuf>)>>`. A pass is due in `notebook_tick` when `looked_from` changes, when a listing arrives that differs, and every `STRANDED_RECHECK` while `stranded_seen` isn't empty, since listing changes alone wouldn't come back 10 minutes later. A pass runs on a thread: `find_stranded`, and `reconcile` of what the main thread said was ready, passing the open path as of the start of the pass, so that nothing open is touched. On a result, update `stranded_seen`, refresh the listing, and if anything's in `Done`, set `self.notice`, naming each and saying where a retired one is. `opened_there` says its undo history is in the retired file.

`sync_logs` handles `outcome.stranded`: add `stranded_mentioned: bool`, reset with `orphan_mentioned`, showing the exact notice once.

Docs: the notebook spec's sections say what was built, its `.kame.retired` becomes `.X.retired.kame` with the reason, and section 13's "aren't built" goes. Followups loses the Move item. `docs/keybindings.md` is Task 7's.

- [ ] **Step 4: Run the tests and the build**

Run: `make all && cargo test`
Expected: all pass.

- [ ] **Step 5: Commit** `feat: clean up databases left behind by a move or delete elsewhere`

---

### Task 7: The menu, dialogs and report

**Files:**
- Modify: `src/notebook.rs` (`show_browser`, new dialogs, report), `src/main.rs` (`dialog_open`, `BackStack`, the dialog dispatch near `:5168`, the close-all near `:2500`)
- Modify: `docs/keybindings.md` (replacing "To move or delete a document in a folder with revision logs…", and saying where a retired database is)

**Interfaces:**
- Consumes: `manage::{check_new_path, delete_document, plan_move, carry_out, MoveReport}`.
- Produces: `path_selection(relative: &str, rename: bool) -> Range<usize>` in `src/notebook.rs`, in characters, as egui counts them.

- [ ] **Step 1: Write the failing tests** in `src/notebook.rs`'s tests, and for `BackStack` in `src/main.rs`'s:

```rust
#[test]
fn rename_selects_the_name_and_move_selects_the_folders() {
    assert_eq!(path_selection("a/b/doc.kame", true), 4..7);
    assert_eq!(path_selection("a/b/doc.kame", false), 0..3);
    assert_eq!(path_selection("doc.kame", true), 0..3);
    assert_eq!(path_selection("doc.kame", false), 0..0);
}

#[test]
fn the_selection_counts_characters_not_bytes() {
    assert_eq!(path_selection("é/matière.kame", true), 2..9);
    assert_eq!(path_selection("é/matière.kame", false), 0..1);
}
```

`back_stops_follow_a_move`: a `BackStack::moved(&mut self, from: &Path, to: &Path)` rewrites matching stops and leaves others.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --bin kame path_selection; cargo test --bin kame back_stops`
Expected: compile errors.

- [ ] **Step 3: Implement.**

In `show_browser`, a `.context_menu` on `RowKind::Entry` rows where `entry.kind == Kind::Document`, with Rename…, Move… and Delete…. No menu for folders or `Kind::File`.

Add `nb.file_op: Option<FileOp>`, where `FileOp` is `Path { from, field, refusal, rename, focus }` or `Delete { path, title }`, and `nb.move_report: Option<MoveReport>`. All three join `dialog_open()` and its close-all. Dialogs follow `show_snapshot_prompt`'s pattern (`:5270`), including its guard that a chord and an Enter in the same frame don't submit, and focus is requested once with the selection set from `path_selection`. A refusal from `check_new_path` shows in the dialog and leaves it open.

Delete: `save_position_now` and `sync_logs` first. If it's the open document, go to `Notebook::index` if that exists and isn't this one, else `adopt_session(Session::anonymous()?)`. Then `delete_document`. Then forget it in `back`, `nb.recent`, `last_document` (if it's that) and `nb.fresh`, and `refresh_listing`. A failure sets `session.last_error`.

Rename or move: refuse with a notice if it's open and `save_failed` or `newer`. `save_position_now`, `sync_logs`, make sure `self.device` is set (factor that out of `sync_logs`), and call `plan_move` with `open = Some((path, document))`. If the moving document is open, remember caret and `page_scroll`, then `adopt_session(Session::anonymous()?)` so that no connection crosses the rename. `carry_out`, passing `Some(&mut self.session)` only when the open document isn't the moving one. If it was open, `open_document(to)` (or `from` on failure), restore caret and `pending_scroll`. Then `BackStack::moved`, `nb.recent`, clear `nb.fresh` if it was this one (it's no longer disposable once moved), set `nb.looked_from = None` so that listing and backlinks refresh, and sync the open document if it changed.

Report: `self.notice` if `left` is empty (`moved to {relative}, and updated links in {n} documents`). Otherwise a window in the shape of `publish.rs`'s report (`:166`) listing each `Left`.

`keybindings.md`: under the notebook's browser, say what the menu does, that a rename or move rewrites links, that links are left in `.md` files, that overwriting is refused, that delete is permanent, what a stale copy on another machine becomes, and how to find it (`ls -a`).

- [ ] **Step 4: Run the tests and the build**

Run: `make all && cargo test`
Expected: all pass.

- [ ] **Step 5: Verify on the app.** Using the `run` skill, make a scratch notebook with a copy of nothing real, under this session's scratchpad. Use `.kame-logs`, four documents with links between them, one in a subfolder, an image, and a `.md` linking in. Build with `cargo build` and open it. Then, with a screenshot of each result:

1. Right-click a document row: the menu shows three items. Right-click a folder row and an image: no menu.
2. Rename with the document open: caret and scroll stay, the window title changes, a linked document leads to it, and "Linked from" still shows.
3. Move to a new folder, with the document closed: the folders show in the browser and its links and image still work.
4. Move over an existing file: the refusal shows and the dialog stays open.
5. Report: the `.md` is named.
6. Delete the open document: confirm once, land on contents. `ls -a` shows nothing left of it, and following a link to it offers a new document that's removed if left unwritten.
7. Delete the root `Notebook.kame`, with it open: a new blank document, and Home says there's none.

Say what you saw for each. If no way to drive the window is available, stop and say so, and don't claim this step.

- [ ] **Step 6: Commit** `feat: rename, move and delete documents from the notebook browser`
