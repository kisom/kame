# Follow-ups

Open work, most pressing first. Delete items as they land.

## Bugs

Found while writing `docs/spec/`, most pressing first. Each spec's last section has the detail.

- [ ] **Save As tells a saved document what exists on this machine (accepted for now).** A relative `Link` is rewritten only if what it names is there, and
  nothing limits that to the notebook, so a document from elsewhere that comes back after a Save As to another depth shows whether its
  paths exist here. No contents are read. It should only ask about what resolves inside the notebook, or the document's own folder.

## v1.0.0

kame is feature complete for 1.0. What's left is to secure the compatibility promise, to build confidence that it doesn't
lose words, and to polish it, in that order, since each rests on the one before. A new feature waits for the roadmap below,
unless daily writing finds something that fails it.

### Compatibility

Releases now put different versions on different machines (Homebrew, the GitHub release and the AUR), so a file or a log
written by one version has to be safe in another.

- [ ] **Decide what sync promises at 1.0.** Revision logs are built and in trial on `notebook-test`, opted in by folder
  (`docs/superpowers/specs/2026-11-03-sync-design.md`). Still to learn: how often conflict notes appear in real writing, and
  whether snapshots should travel in the logs. Either they're stable at 1.0, or they stay opt-in and say they're a trial. Then
  whether logs keep growing (nothing compacts them, and every merge stores both whole documents in the undo log), or 1.0 needs
  a way to compact them.

- [ ] **A field written at its default opens as a newer version's.** A field this build knows and leaves out at its
  default, such as `centered`, `align`, `link`, `language`, `removed` or `clipped`, written by another program at that
  default, is gone when the JSON is read and written again, so the document opens read-only as one from a newer version,
  as an unknown field would. Nothing is lost, but it says to update kame for no reason. Only a list item's `depth` is
  taken as this build's own. The bounds in `model/document.rs` could list every such field.

### Reliability

- [ ] **A stretch of daily writing with no lost words,** on two machines at different versions, with sync on. How long is
  still to agree.
- [ ] **A stale database from before places were recorded,** whose logs left before a pass first saw it, has no place, so it isn't
  recognised as stale until it's next synced, and opening one on another machine after a delete can bring the document back, as it
  did before. A pass stamps every other one.
- [ ] **A list item moved on one machine goes back after a later edit on another.** A merge takes the order of the later
  side, so a move is undone by a revision written later elsewhere, even one that only edited words, with no note. Nothing is
  lost. A three-way order rule would keep it, but it changes merged bytes between versions, so it's for before 1.0 or not at
  all. See `docs/spec/merging.md`, section 9.
- [ ] **A partial log folder after a move.** If a machine edits X offline while another renames X to Y, its surviving
  `.X.kame.sync/` reaches the other as a logs-only X with no usable head. Its X isn't stranded, so its edits never reach Y, and
  X can come back as an empty document. It's divergence and not loss. See "Accepted limits" in the file operations design.

### Polish

- [ ] **Rough edges daily writing finds,** added here as they're found.
- [ ] **The long fuzzing runs are noisy.** Undo and redo refuse a step that no longer fits by catching its panic, and
  the default panic hook prints each one, so a long run's output is mostly those lines, and a failure is hard to find in
  it. A quiet hook for the long runs, printing only what fails, would help.

Performance, worth doing if daily writing feels it. Measured with `src/perf.rs` and `sample` on an M3 Max, on a 2,500-word document and a copy ten times over. Typing takes 2–4 ms a
keystroke in the page, a little more for a long paragraph, and nothing yet is close to a frame. On macOS, Metal keeps about 450 MB
of buffers in use while frames come every second or so and lets them go a moment after they stop, as a bare eframe window does. An
idle window wakes every 3 seconds, so it lets them go, and holds about 255 MB with a 3,000-word document. On Linux, on talos (Core
i3-1315U, Intel graphics) with a 3,571-word document and spelling off, kame holds 146 MB resident, 49 MB of it its own heap, and
250 MB of GPU buffers in system memory, which the driver keeps whether or not frames come. Idle, it takes 0.01 s of CPU a minute.

- [ ] **Spell checking holds about 130 MB,** 117 MB of it Harper's dictionary as Harper builds it. It's loaded only while checking
  is on. A smaller footprint would mean a lighter dictionary from Harper, or our own.
- [ ] **A keystroke sets its whole paragraph again.** It redraws only the lines whose drawing changed: in a 390-word paragraph
  at 2x, about 50 of the image's 1,137 rows, and one keystroke in nine still draws it whole, when the number of lines or the
  ink's bounds change. In `src/perf.rs`'s frames, which upload nothing, shaping it and choosing its lines is 2.3 ms of a 2.9 ms
  keystroke; the upload, timed apart, is about 0.25 ms for a band and 1.3 ms whole. Each line is chosen over up to three lines
  as first fit sets them, its own and two more, so an edit can move lines above it: of 3,918 edits tried in three texts at
  three measures, 332 moved the line just above and 2 moved one further up. Setting only from a line or two above the edit would
  need to know when that's enough.
- [ ] **Opening shapes every paragraph to know how tall the page is:** 140 ms for 25,000 words. Estimating the height of paragraphs
  far from the screen would make opening independent of length.

## Roadmap (after 1.0)

- [ ] **An Intel or universal build of the app.** Not feasible for now. It would be a second target in the release
  workflow. The app stays ad-hoc signed and not notarized (an Apple Developer ID isn't worth its yearly fee here), and
  the Homebrew cask takes it out of quarantine as it installs.
- [ ] **Tables.** The model has no equivalent. Tables are wanted but too complex for now;
  the stopgap is automatic, as a Markdown table imports as a `markdown` code block holding the table's exact
  Markdown, ready to be copied into a table block once there is one.
- [ ] **PDF export.** On hold since the first design, and not needed for 1.0.
- [ ] **Footnotes in the site's margins.** Sidenotes beside their markers on wide screens, so
  readers don't jump to the foot and back. Held back because kame already separates the two: a
  marginal belongs beside the text, and a footnote is there if wanted. Long footnotes would also
  stack and drift, and on narrow screens they'd have to fold back into the text. Worth trying if
  footnotes on the live site start to feel like they belong in the margin, most likely as a
  `site.yaml` setting, changing only how a page lays them out and not what kame stores.
- [ ] **Several windows open at once.** Needs a design. Each kame has one window today, and several can run, each holding
  the document it has open so that no other opens it; `instance.lock` only keeps a later one from going back to the last
  document. The choice is between windows in one process, as egui's viewports allow, and several processes. Then what they
  share: the notebook, settings, sync and the checks for files changing on disk.
- [ ] **An essay's thesis and arguments in the left gutter.** The margin on the right holds marginals beside their blocks;
  the gutter on the left could hold what the essay argues, its thesis and the arguments for it, to keep in view while
  writing. Needs a design: what's kept there, whether it belongs to blocks or to the document, and whether it exports.
- [ ] **Other sync tools' conflict copies.** Only Syncthing's are recognised. Revision logs don't depend on this. See
  `docs/spec/merging.md`, section 7.
