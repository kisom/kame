# Follow-ups

Open work, most pressing first. Delete items as they land.

## Bugs

Found while writing `docs/spec/`, most pressing first. Each spec's last section has the detail.

- [ ] **A merge can hang the app.** If a merge's revision is in the logs but a block it needs hasn't
  arrived, the sync loop never ends, on the interface thread. `merging.md`, section 9.
- [ ] **An unusable last revision.** If the revision a document last settled on is in the logs but can't
  be used, reading it panics, or the page follows another version. `merging.md`, section 9.
- [ ] **A torn last line swallows the next revision.** Appending to a log doesn't check that it ends
  in a line feed. `merging.md`, section 9.
- [ ] **A dangling symbolic link can lead out of a notebook.** Following a link to one creates a file at
  its target. `notebook.md`, section 9.
- [ ] **Typing on an image stores text in it.** `document-format.md`, section 8.
- [ ] **`kame-sync enable` prints too little.** It leaves out `*.kame-wal` and `*.kame-shm`. Also,
  check `.stignore` on each machine. `merging.md`, section 6.1.
- [ ] **New fields stop older builds reading logs, with no message.** A centered or right-aligned image
  now does this, and any new field or kind will. Needs a decision on how logs handle versions.
  `merging.md`, section 9.
- [ ] **Smaller ones in the specs' weak points:** a logs-only document without `.kame-logs` opens blank,
  a folder's index with only logs is overwritten when followed, `.markdown` files are unread outside
  Open, links that publish private names, read-only opens not checking `user_version`, and two
  programs writing one file.

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

- [ ] **The Move command,** with the clean-up of databases a move leaves behind. Moving a document in
  a folder with revision logs leaves a stale copy on the other machine for now.
- [ ] **A command palette.**
- [ ] **File > Link to Document… in a notebook.** It writes `~/...` for anything under the home folder,
  which breaks when the notebook moves. It could keep the way from the document, as images now do.

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
