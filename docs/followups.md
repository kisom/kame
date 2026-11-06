# Follow-ups

Open work, most pressing first. Delete items as they land.

## Bugs

Found while writing `docs/spec/`, most pressing first. Each spec's last section has the detail.

- [ ] **Save As tells a saved document what exists on this machine (accepted for now).** A relative `Link` is rewritten only if what it names is there, and
  nothing limits that to the notebook, so a document from elsewhere that comes back after a Save As to another depth shows whether its
  paths exist here. No contents are read. It should only ask about what resolves inside the notebook, or the document's own folder.

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

## References

Built and driven by hand, with these left. See `docs/superpowers/specs/2026-11-04-references-design.md`.

- [ ] **`src/references.rs` could split,** and `ReferenceForm` could go in a library, out of the app.

## Performance

Measured with `src/perf.rs` and `sample` on an M3 Max, on a 2,500-word document and a copy ten times over. Typing takes 2–4 ms a
keystroke in the page, a little more for a long paragraph, and nothing yet is close to a frame. On macOS, Metal keeps about 450 MB
of buffers in use while frames come every second or so and lets them go a moment after they stop, as a bare eframe window does. An
idle window wakes every 3 seconds, so it lets them go, and holds about 255 MB with a 3,000-word document. On Linux, on talos (Core
i3-1315U, Intel graphics) with a 3,571-word document and spelling off, kame holds 146 MB resident, 49 MB of it its own heap, and
250 MB of GPU buffers in system memory, which the driver keeps whether or not frames come. Idle, it takes 0.01 s of CPU a minute.

- [ ] **Spell checking holds about 130 MB,** 117 MB of it Harper's dictionary as Harper builds it. It's loaded only while checking
  is on. A smaller footprint would mean a lighter dictionary from Harper, or our own.
- [ ] **A keystroke sets its whole paragraph again.** It redraws only the lines whose drawing changed, so in a 390-word paragraph
  at 2x shaping it and choosing its lines is now 2.2 ms of a 2.8 ms keystroke. Lines are chosen a line ahead, so those before
  the one above the edit should stay as they were, but they are set again all the same.
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
- [ ] **Several windows open at once.** Needs a design. kame runs as one instance today, held by `instance.lock`, so the
  choice is between windows in one process, as egui's viewports allow, and several processes. Then what they share: the
  notebook, settings, sync and the checks for files changing on disk. And what happens when the same document is open in two.
- [ ] **An essay's thesis and arguments in the left gutter.** The margin on the right holds marginals beside their blocks;
  the gutter on the left could hold what the essay argues, its thesis and the arguments for it, to keep in view while
  writing. Needs a design: what's kept there, whether it belongs to blocks or to the document, and whether it exports.
