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
- [ ] **A snapshot taken before a Save As to another folder still says its links and images from the old one.** Save As says them
  again through the history, but leaves snapshots as they were taken, so restoring one brings back paths that lead from where the
  document was.

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

- [ ] **A corrupt log line stays for good.** Logs are only added to, so a line that doesn't parse, and isn't torn, stays until it's
  removed by hand. Until then a document it could belong to can't be retired as having no home. A notice says which logs.
- [ ] **A stale database from before places were recorded,** whose logs left before a pass first saw it, has no place, so it isn't
  recognised as stale until it's next synced, and opening one on another machine after a delete can bring the document back, as it
  did before. A pass stamps every other one.
- [ ] **A partial log folder after a move.** If a machine edits X offline while another renames X to Y, its surviving
  `.X.kame.sync/` reaches the other as a logs-only X with no usable head. Its X isn't stranded, so its edits never reach Y, and
  X can come back as an empty document. It's divergence and not loss. See "Accepted limits" in the file operations design.
- [ ] **A rename shadowing a link.** A link `b` that led to `b.md`, or offered to create `b.kame`, opens the moved
  document after something is renamed to `b.kame`.
- [ ] **A move into a new folder shadowing a link.** A folder is looked at before a document of the same name, so moving
  something to `b/a.kame`, which makes the folder `b/`, leaves a link `b` that led to `b.kame` offering to create `b/Notebook.kame`.
- [ ] **A command palette.**

## References

Built and driven by hand, with these left. See `docs/superpowers/specs/2026-11-04-references-design.md`.

- [ ] **`src/references.rs` could split,** and `ReferenceForm` could go in a library, out of the app.

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

## Export

- [ ] **PDF export (on hold).** Not required for v1.

## Ideas

- [ ] **Footnotes in the site's margins.** Sidenotes beside their markers on wide screens, so
  readers don't jump to the foot and back. Held back because kame already separates the two: a
  marginal belongs beside the text, and a footnote is there if wanted. Long footnotes would also
  stack and drift, and on narrow screens they'd have to fold back into the text. Worth trying if
  footnotes on the live site start to feel like they belong in the margin, most likely as a
  `site.yaml` setting, changing only how a page lays them out and not what kame stores.
