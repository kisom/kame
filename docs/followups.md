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
- [ ] **Save As's rewrite of links and images is an invisible undo step, and it ends the redo tail.** Undoing it changes nothing on the
  screen, and leaves a link that's wrong from the new folder until it's redone, and anything waiting for a redo can't come back once it's run.
- [ ] **On Save As, a `Link` to a document that's there only as logs stays as written.** Only what's on disk counts as what it names, and core can't
  see logs, so the link stops leading to the document from another folder. A `DocumentLink` isn't affected.

- [ ] **A `www.` address in capitals, or a real file named `www.x.kame`.** `WWW.Example.com/x` in a `Link` span still makes a folder and a
  document, as only lowercase counts as a web address. A `DocumentLink` to a real `www.x.kame` stays as written on Save As to another
  folder, and clicking it then makes an empty one. `leads_within_kame`, `retargeted`, `names_document` and `link_ranges` need one
  case-insensitive answer, and to know a link's kind.
- [ ] **A link to itself goes stale if the file has gone by the time of Save As.** `names_document` compares canonical paths, and a missing
  file has none, so the link then leads to where the file was.
- [ ] **Save As tells a saved document what exists on this machine (accepted for now).** A relative `Link` is rewritten only if what it names is there, and
  nothing limits that to the notebook, so a document from elsewhere that comes back after a Save As to another depth shows whether its
  paths exist here. No contents are read. It should only ask about what resolves inside the notebook, or the document's own folder.

## Annotations

- [ ] **Moving a marginal.** The only way today is to remove it and add it again on the other block.

## Search

- [ ] **Ctrl-F only searches the body.** Marginals, notes and footnotes are only searched by Ctrl-R,
  which needs a notebook, so a document on its own can't search them.

## Notebook

Built as a proof of concept. See section 13 of `docs/superpowers/specs/2026-11-03-notebook-design.md`.

- [ ] **A notebook opened through a symbolic link can't create a document from a link.** Its root is the real path and the document's isn't, so
  `Notebook::holds` says no to anything not there yet, and a click says "no document at" it. `/tmp` and `/var` on macOS are such paths.
- [ ] **A corrupt log line stays for good.** Logs are only added to, so a line that doesn't parse, and isn't torn, stays until it's
  removed by hand. Until then a document it could belong to can't be retired as having no home. A notice says which logs.
- [ ] **A stale database from before places were recorded,** whose logs left before a pass first saw it, has no place, so it isn't
  recognised as stale until it's next synced, and opening one on another machine after a delete can bring the document back, as it
  did before. A pass stamps every other one.
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

## References

Built and driven by hand, with these left. See `docs/superpowers/specs/2026-11-04-references-design.md`.

- [ ] **Citations a rename can't see.** One only in another log head or unmerged logs stays `[old]`.
- [ ] **Feed citations use fragment-only links,** as its footnotes do, so they lead nowhere off the page.
- [ ] **`page_blocks` clones the blocks every frame** when one holds a citation. It hasn't been measured.
- [ ] **`src/references.rs` could split,** and `ReferenceForm` could go in a library, out of the app.
- [ ] **`stamp` and `load` run on the UI thread,** and a page's export works out `cited` twice.
- [ ] **What's wrong with a reference file** can vanish from `last_error` or go stale there.
- [ ] **A name holding a semicolon,** possible only by hand in its file, splits on Save from the form.
- [ ] **With no file open, a rename can lose the old key** if the unsaved copy can't follow. It needs a failed write to the temporary database, and the reference survives under the new key.
- [ ] **A new single name such as `King, Jr.`** splits on commas unless it's typed with a `;`.

## Performance

Measured with `src/perf.rs` and `sample` on an M3 Max, on a 2,500-word document and a copy ten times over. Typing takes 2–4 ms a
keystroke in the page, a little more for a long paragraph, and nothing yet is close to a frame.

- [ ] **Spell checking holds about 130 MB,** 117 MB of it Harper's dictionary as Harper builds it. It's loaded only while checking
  is on. A smaller footprint would mean a lighter dictionary from Harper, or our own.
- [ ] **Memory on Linux hasn't been measured.** On macOS, Metal keeps about 450 MB of buffers in use while frames come every second
  or so and lets them go a moment after they stop; a bare eframe window does the same. An idle window wakes every 3 seconds, so
  it lets them go, and holds about 255 MB with a 3,000-word document.
- [ ] **A keystroke redraws its whole paragraph's image, then converts it for upload.** Drawing straight into egui's premultiplied
  form would save a pass, about a sixth of a keystroke on a Retina screen. Drawing only the lines that changed would save more.
- [ ] **Opening shapes every paragraph to know how tall the page is:** 140 ms for 25,000 words. Estimating the height of paragraphs
  far from the screen would make opening independent of length.
- [ ] **Every frame fingerprints every block, twice,** even when nothing changed. A revision number on the document would skip it.

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
- [ ] **A relative link in an imported file breaks when it's first saved in another folder.** Until then it leads from the Markdown
  file, and Save As only says links again from a document's own earlier file, which an import doesn't have.

## Export

- [ ] **PDF export (on hold).** Not required for v1.

## Ideas

- [ ] **Footnotes in the site's margins.** Sidenotes beside their markers on wide screens, so
  readers don't jump to the foot and back. Held back because kame already separates the two: a
  marginal belongs beside the text, and a footnote is there if wanted. Long footnotes would also
  stack and drift, and on narrow screens they'd have to fold back into the text. Worth trying if
  footnotes on the live site start to feel like they belong in the margin, most likely as a
  `site.yaml` setting, changing only how a page lays them out and not what kame stores.
