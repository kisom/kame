# Follow-ups

Open work, most pressing first. Delete items as they land.

## Bugs

Found while writing `docs/spec/`, most pressing first. Each spec's last section has the detail.

- [ ] **Before the next field on `Metadata` or `Annotation`, stop it invalidating every log.** `Revision::id()` hashes the
  re-serialized struct, so adding a field makes every line of every log fail its check, and they drop out of every merge. Checking the
  digest against the line's raw `revision` bytes would validate every line ever written. It needs its own task, since it brings old
  revisions back into merges. `skip_serializing_if` on new fields is the cheap stopgap. This is the most pressing item here.
- [ ] **Check `.stignore` on each machine** for `*.kame-wal` and `*.kame-shm`. `kame-sync enable` prints
  them now, but folders already turned on won't have been told.
- [ ] **A footnote or note added right after punctuation** has no word to anchor to, so it sits at one point and its mark moves along as you keep typing there.
- [ ] **Typing straight after a paste** joins its undo step, so undo takes away both. `Session::insert_as_step` is what a cited key uses to stay apart.

## Site

- [ ] **A missing image publishes its local path.** A published page's `src` gets the path on this
  machine, such as `/home/you/notes/images/gone.png`, though the build does warn that it's missing. It
  should publish as nothing, or as its words.

## Annotations

- [ ] **A key for adding a marginal.** Notes and footnotes have one, and a marginal is only on the
  Annotate and right-click menus. Feedback suggested Ctrl-M, but Cmd-M minimises a window on a Mac.
- [ ] **Moving a marginal.** The only way today is to remove it and add it again on the other block.
- [ ] **A marginal that was never written.** Emptying a marginal removes it, but adding one and
  pressing Close without typing leaves an empty one in the document.

## Help

- [ ] **The F1 dialog should be landscape, not portrait.** Some of it seems to run off the screen as
  it is. Laying it out wide would make room without shrinking anything.
- [ ] **The F1 dialog is missing commands:** at least Ctrl-N, Ctrl-A, Ctrl-[, F7, and typing `|` or a
  code fence. It doesn't scroll and is already taller than the smallest window, so it needs to scroll
  before it gets more rows.

## Search

- [ ] **Ctrl-F only searches the body.** Marginals, notes and footnotes are only searched by Ctrl-R,
  which needs a notebook, so a document on its own can't search them.

## Notebook

Built as a proof of concept. See section 13 of `docs/superpowers/specs/2026-11-03-notebook-design.md`.

- [ ] **A corrupt log line stays for good.** Logs are only added to, so a line that doesn't parse, and isn't torn, stays until it's
  removed by hand. Until then a document it could belong to can't be retired as having no home. A notice says which logs.
- [ ] **A stale database from before places were recorded,** whose logs left before a pass first saw it, has no place, so it isn't
  recognised as stale until it's next synced, and opening one on another machine after a delete can bring the document back, as it
  did before. A pass stamps every other one.
- [ ] **A document with logs that can't be read never syncs and doesn't say why.** `src/main.rs` leaves `logged` as `None` when
  `Logged::open` fails, with no notice. Not new.
- [ ] **A partial log folder after a move.** If a machine edits X offline while another renames X to Y, its surviving
  `.X.kame.sync/` reaches the other as a logs-only X with no usable head. Its X isn't stranded, so its edits never reach Y, and
  X can come back as an empty document. It's divergence and not loss. See "Accepted limits" in the file operations design.
- [ ] **A stranded document open on another document's logs reads them on every sync check,** on the main thread, since logs
  replaced while it's open always look like news to it, and a document *opened* stranded on them never has a first sync, so it
  runs a full `Store::load` at each check. A document held off with `others_logs` reads what's new at each check too.
- [ ] **News for a healthy open document reads its logs on the main thread,** in `is_stranded`. It's only when there's news,
  and fine for small logs.
- [ ] **`.X.moving-N.kame` doesn't go with its document.** A temporary name left by a crash isn't in `belonging_files`, so a
  delete or move leaves it.
- [ ] **A retired copy's `-wal` stays behind.** `belonging_files` doesn't list what SQLite keeps beside a retired copy, so if a
  crash left one, a delete leaves it, and a move leaves the words in it under the old name.
- [ ] **A rename shadowing a link.** A link `b` that led to `b.md`, or offered to create `b.kame`, opens the moved
  document after something is renamed to `b.kame`.
- [ ] **A command palette.**
- [ ] **File > Link to Document… in a notebook.** It writes `~/...` for anything under the home folder,
  which breaks when the notebook moves. It could keep the way from the document, as images now do.

## References

Built and driven by hand, with these left. See `docs/superpowers/specs/2026-11-04-references-design.md`.

- [ ] **Citations a rename can't see.** One only in another log head or unmerged logs stays `[old]`.
- [ ] **Feed citations use fragment-only links,** as its footnotes do, so they lead nowhere off the page.
- [ ] **`page_blocks` clones the blocks every frame** when one holds a citation. It hasn't been measured.
- [ ] **`src/references.rs` could split,** and `ReferenceForm` could go in a library, out of the app.
- [ ] **The browser panel and File > Open** close a form and lose what's typed in it.
- [ ] **A note opened over a form:** Escape closes the form first.
- [ ] **Clicking a form under a link prompt** puts it on top, and Enter still applies the prompt.
- [ ] **A followed link refused during a rename** leaves a junk stop on the way back.
- [ ] **`stamp` and `load` run on the UI thread,** and a page's export works out `cited` twice.
- [ ] **What's wrong with a reference file** can vanish from `last_error` or go stale there.
- [ ] **A clicked citation's entry** stays where it opened while the page scrolls.
- [ ] **A name holding a semicolon,** possible only by hand in its file, splits on Save from the form.
- [ ] **With no file open, a rename can lose the old key** if the unsaved copy can't follow. It needs a failed write to the temporary database, and the reference survives under the new key.
- [ ] **A new single name such as `King, Jr.`** splits on commas unless it's typed with a `;`.
- [ ] **Test gaps:** paste leaving no note, `chmod` as root.

## Sync

- [ ] **Revision logs before a real folder.** Designed in `docs/superpowers/specs/2026-11-03-sync-design.md`.
  They're built, and in trial on `notebook-test`, opted in by folder. Still to learn: how often conflict
  notes appear in real writing, and whether snapshots should travel in the logs.

## Typography

- [ ] **Adjust the word spacing numbers with use.** They're in `layout/breaks.rs`. The study is in
  section 14 of `docs/superpowers/specs/2026-11-03-word-spacing-design.md`.

## Markdown import

- [ ] **Tables (deferred).** Simplified on import because the model has no equivalent.
  Tables are wanted but too complex for now, and a code block is the stopgap.
- [ ] **A link to a heading stays an ordinary link.** `[x](<#Heading>)` imports as a web link, so
  clicking it goes to the browser, where typing it would make a document link.

## Export

- [ ] **PDF export (on hold).** Not required for v1.

## Ideas

- [ ] **Footnotes in the site's margins.** Sidenotes beside their markers on wide screens, so
  readers don't jump to the foot and back. Held back because kame already separates the two: a
  marginal belongs beside the text, and a footnote is there if wanted. Long footnotes would also
  stack and drift, and on narrow screens they'd have to fold back into the text. Worth trying if
  footnotes on the live site start to feel like they belong in the margin, most likely as a
  `site.yaml` setting, changing only how a page lays them out and not what kame stores.
