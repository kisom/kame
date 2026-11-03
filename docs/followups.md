# Follow-ups

Open work, most pressing first. Tick items off or delete them as they land.

## Typography

- [ ] **Word spacing.** Justified text leaves wide gaps because cosmic-text takes each line as it
  comes and can only widen spaces. Studied on 586 devlog paragraphs through kame's own shaping, with a
  model of first fit that matched the engine's line breaks on all 586 at type 16, width 640:

  | Spaces over | Today | Spaces may close to 80% | Best breaks over the paragraph | Same, with hyphens |
  |---|---|---|---|---|
  | 1.5x | 28.8% | 13.1% | 12.2% | 3.4% |
  | 2x | 5.7% | 2.2% | 1.5% | 0.6% |
  | 3x | 0.5% | 0.3% | 0.1% | 0.1% |

  Breaking over the paragraph moves lines further up: for 2.1% of words typed and 7.6% of words
  removed in revising, against none for the two line-at-a-time rules. Hyphens land on about one line
  in ten. Letting spaces close looks like the first step, since it gets most of the gain and keeps the
  page still. Any of these needs kame to set its own lines: shape the paragraph once on a single line,
  choose the breaks, and build `runs` and `lines` from it. Painting and hit-testing already read those
  rather than cosmic-text's lines, and a fit took under a millisecond. Hyphens could be drawn without
  entering the text, so they wouldn't touch byte offsets, but they would need `hypher` and a language.
  Write a spec before building.

## Markdown import

- [ ] **Tables (deferred).** Simplified on import because the model has no equivalent.
  Tables are wanted but too complex for now, and a code block is the stopgap.

## From earlier feedback

- [ ] **PDF export (on hold).** Still open in `feedback2.txt`; not required for v1.

## Sync

- [x] **Syncthing can replace a `.kame` file while it is open.** kame now notices, stops saving
  to it, and asks whether to merge the two, save this copy elsewhere or open the one on disk.

## Ideas

- [ ] **Sync for drift between one person's machines.** Designed and prototyped: see
  `docs/superpowers/specs/2026-11-03-sync-design.md` and `crates/kame-sync`. It merges document
  states by block and sentence, keeps a conflict's other version in a note, and carries revisions in
  logs that each have one writer. The design is accepted. The app merges conflict copies and replaced
  files; revision logs are the next stage.

- [ ] **Footnotes in the site's margins.** Sidenotes beside their markers on wide screens, so
  readers don't jump to the foot and back. Held back because kame already separates the two: a
  marginal belongs beside the text, and a footnote is there if wanted. Long footnotes would also
  stack and drift, and on narrow screens they'd have to fold back into the text. Worth trying if
  footnotes on the live site start to feel like they belong in the margin, most likely as a
  `site.yaml` setting, changing only how a page lays them out and not what kame stores.
- [ ] **A kame notebook.** The long-term goal, and notes are in `MultiKame.kame`: an
  extended kame for a binder of kame and Markdown documents, with search across them, quick
  linking and publishing by path.
