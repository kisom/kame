# Follow-ups

Open work, most pressing first. Delete items as they land.

## Bugs

Found while writing `docs/spec/`, most pressing first. Each spec's last section has the detail.

- [ ] **Check `.stignore` on each machine** for `*.kame-wal` and `*.kame-shm`. `kame-sync enable` prints
  them now, but folders already turned on won't have been told.

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

- [ ] **A clean-up pass stops without a word** on any log it can't read, one from a newer kame, or one with a whole line
  that can't be checked, since any of those could hold the revision being looked for. The user is told nothing, and stranded
  databases stay until it's fixed. Such a line stays for good, as logs are only added to: every line written before a `Metadata`
  field was added, and a line torn by a crash that another was then written after. So a notebook with one has no clean-up at all
  until something rewrites or removes it.
- [ ] **A stale database from before places were recorded.** It has no place, so it isn't recognised as stale until it's next
  synced, and opening one on another machine after a delete can bring the document back, as it did before. Once every
  machine has synced its documents, there are none.
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
