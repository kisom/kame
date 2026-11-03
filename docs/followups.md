# Follow-ups

Open work, most pressing first. Tick items off or delete them as they land.

## Typography

- [x] **Word spacing.** Justified text leaves wide gaps because cosmic-text takes each line as it
  comes and can only widen spaces. Studied through kame's own shaping at type 16, width 640, on two
  essays (68 and 83 lines) and 586 devlog paragraphs, with a model of first fit that matched the
  engine's breaks on all but one paragraph. Each cell reads essay / essay / devlog:

  | Rule | Spaces over 1.5x | Spaces over 2x | Words typed that move an earlier line | Words removed that move a line above |
  |---|---|---|---|---|
  | Today | 30 / 27 / 29% | 6.7 / 7.4 / 5.7% | none | none |
  | Spaces may close to 80% | 15 / 14 / 13% | 3.3 / 1.5 / 2.2% | none | none |
  | Look one line ahead | 12 / 7.5 / 13% | 0 / 1.5 / 1.7% | 0.3 / 0.8 / 0.3% | 1.2 / 0 / 0.2% |
  | Look two lines ahead | 8.3 / 6.1 / 12% | 0 / 0 / 1.5% | 1.0 / 1.7 / 0.7% | 6.4 / 6.6 / 2.4% |
  | Best breaks over the paragraph | 5 / 6.1 / 12% | 0 / 0 / 1.5% | 2.9 / 2.8 / 1.0% | 19 / 9.4 / 3.6% |
  | Same, with hyphens | 1.7 / 0 / 3.4% | 0 / 0 / 0.6% | 5.2 / 3.9 / 1.9% | 11 / 8.9 / 4.8% |

  The last two columns skip the lines nearest the edit. On the essays one line is over 1%, so small
  differences mean little. Looking one line ahead is the likely choice: it gets most of the gain, a
  line depends only on the text up to a little past it, and the look-ahead is one constant to adjust.
  Closing up alone can push a gap further down rather than remove it. All of these need kame to set
  its own lines: shape the paragraph once on a single line, choose the breaks, and build `runs` and
  `lines` from it. Production code reads the engine's lines only in `layout/text.rs`, though many
  tests read them, and a fit took under a millisecond. Hyphens would be drawn without entering the
  text, so they wouldn't touch byte offsets, but need `hypher` and a language; a word too long to fit
  is the case only they fix, so judge them after the rest. How the movement feels can only be judged
  in the app. The study left out the room footnote marks take. Designed in
  `docs/superpowers/specs/2026-11-03-word-spacing-design.md`. Built and landed, so what's left
  is to adjust the numbers in `layout/breaks.rs` with use.

  Tuned on the two essays, which are the writing this is for, with the devlog only as a check. Best so
  far at type 16: look one line ahead, let spaces close to no less than 90%, and charge a high cost for
  a hyphen (50000 in the study). Spaces over 1.5x fall from 28% to 6.1% and over 2x from 6.8% to none,
  the widest goes from 2.30x to 1.63x, and no line is tighter than 90%. It used 7 hyphens in about 2650
  words, moved an earlier line for 0.5% of words typed and none for words removed. The devlog, which
  played no part in choosing, goes from 29% to 11% and 5.7% to 1.0%. Type 18 gains less: 10% and 0.7%.
  Without hyphens the same rule reaches 13% and 1.5%. The devlog also shows two-letter starts such as
  be-tween and al-ways, so try requiring three letters before a hyphen. The stylesheet's test that says
  kame doesn't hyphenate covers exports, which browsers break for themselves.

## Markdown import

- [ ] **Tables (deferred).** Simplified on import because the model has no equivalent.
  Tables are wanted but too complex for now, and a code block is the stopgap.

## From earlier feedback

- [ ] **PDF export (on hold).** Still open in `feedback2.txt`; not required for v1.

## Sync

- [x] **Syncthing can replace a `.kame` file while it is open.** kame now notices, stops saving
  to it, and asks whether to merge the two, save this copy elsewhere or open the one on disk.

## Code blocks

- [x] **Scroll long code lines sideways instead of wrapping.** Wrapping misleads in code. Feasible, and
  planned after word spacing: stop wrapping code blocks, keep a sideways offset for each, add it wherever
  the block's text, caret and selection are drawn and clicks are mapped, clip at the column, and let
  the caret pull a long line into view. Each block is one image, so a very long pasted line could
  exceed the graphics card's limit and would need to fall back to wrapping. The site only needs a
  stylesheet change, keeping wrapping for print. At 70% size few of the devlog's code lines at type 16
  are too long: 8 of 3640.

## Ideas

- [ ] **Spelling and grammar check.** Typos in essays only get caught by rereading. Marks could use
  `range_rects` and be drawn on the page without touching the text, with a personal word list in the
  config and code skipped. Spelling is a word list. Grammar is the harder half and what turned up in
  the essays: "is has" and "what need" are made of real words, so it takes a language checker or a
  review pass rather than a dictionary.

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
