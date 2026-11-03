# Word spacing — design

Date: 2026-11-03
Status: awaiting review. Section 11 lists the decisions made on your behalf.

## 1. Purpose

Justified paragraphs in kame have lines with visibly wide gaps. The goal is an even page in the
writing kame is for, without the page shifting around as you write and without changing what a
document stores.

## 2. Why it happens

cosmic-text fills each line with as many words as fit at their natural spacing, then widens the spaces
to reach the right edge. It never looks at the next line and it can't close spaces up, so one long word
can leave a line very loose.

## 3. The rule

Set one line at a time, as now, but choose each break with these changes:

- **Look one line ahead.** Choose the break that gives the best fit over this line and the next.
- **Let spaces close a little,** to no less than 90% of normal.
- **Hyphenate sparingly.** A hyphen costs enough that it's used only when it clearly helps.

Because a line depends only on the text up to a little past it, layout stays a pure function of the
text, and an edit rarely moves a line above it.

## 4. What the study found

Measured through kame's own shaping at type 16, width 640. The essays are `CommandingHeights` and
`AgainstBrainrot`, about 130 justified lines between them, and the settings were chosen on them. The
devlog is 586 paragraphs that played no part in choosing. A model of today's rule matched the engine's
line breaks on all but one paragraph.

| | Today | This rule, no hyphens | This rule |
|---|---|---|---|
| Essays: spaces over 1.5x | 28% | 13% | 6.1% |
| Essays: spaces over 2x | 6.8% | 1.5% | none |
| Essays: widest space | 2.30x | 2.20x | 1.63x |
| Essays: lines tighter than 90% | none | none | none |
| Essays: hyphens | none | none | 7 in about 2650 words |
| Essays: words typed that move an earlier line | none | 0.5% | 0.5% |
| Essays: words removed that move a line above | none | none | none |
| Devlog: spaces over 1.5x | 29% | 20% | 11% |
| Devlog: spaces over 2x | 5.7% | 2.6% | 1.0% |

The movement rows skip the lines nearest the edit, which can shift under any rule, and were measured a
word at a time. At type 18 the rule gains less on the essays: 10% and 0.7%.

Choosing breaks over a whole paragraph, as TeX does, was only slightly better on the essays and moved
far more: up to 19% of removed words moved a line above. Looking further ahead moves more too.

## 5. How layout changes

Only `layout_block_marked_with_align` in `crates/kame-typeset/src/layout/text.rs` changes its approach,
and only for justified left-to-right text. Headings, lists, quotes, code, prose blocks, margin notes,
footnotes and right-to-left text stay with the engine.

1. **Shape once, unwrapped.** Give cosmic-text the paragraph with no width limit. Each line of the
   text engine, meaning each piece between line breaks in the block, comes back as one run of glyphs.
   Footnote marks already widen their characters in `styled_spans`, so the fit accounts for them, which
   the study did not.
2. **Find the breaks.** Use `unicode-linebreak`, as the engine does, mapped from byte offsets to glyphs.
   Add hyphenation points (section 7).
3. **Choose the lines** (section 6).
4. **Build `runs` and `lines`.** Move each line's glyphs to start at zero. Drop the spaces at the line's
   end, as the engine does, so an offset among them falls forward to the next line. Then run `hang_line`.
   Its share of width among spaces already works from any starting width, so the same code stretches and
   closes up. It needs its early return removed, and a last line that only fits by closing up has to be
   treated as justified.

**Baselines.** Each line is `line_height` tall, with the baseline at the same distance from its top,
taken from the unwrapped shaping. The engine works this out per line, so a line with inline code can sit
slightly differently today. Lines in a paragraph will now be exactly even.

**Nothing else needs to know.** Production code reads the engine's lines only in `text.rs`; painting and
hit-testing read `runs` and `lines`. The test-only `buffer` field would no longer hold the wrapped lines,
so the tests that read it for multi-line justified paragraphs move to `runs`. About 29 lines of tests
read `buffer`, and most use one-line blocks that are unaffected.

## 6. Choosing a break

For a line from `i` to a candidate break `b`, let `f` be the final width of its spaces as a share of
normal, allowing for the punctuation that hangs:

```text
f = 1 + (measure + hang - natural width) / width of its spaces
r = (f - 1) / 0.5           when f >= 1
r = -(1 - f) / (1 - 0.9)    when f < 1; a break with f < 0.9 isn't allowed
cost = (1 + 100 * |r|^3)^2
```

Added to that:

| Cost | Amount |
|---|---|
| A hyphen | 50000 |
| A second hyphen in a row | 150000, and a third isn't allowed |
| A tight line beside a very loose one | 10000 |
| A last line under 10% of the measure | 30000 |
| A word too wide for any line | 10^13, so only when there is no other way |

The last line of a paragraph costs nothing unless it has to close up.

To set a line: run first fit two lines forward from `i` to find a stopping point, find the cheapest way
to reach it, keep only the first break, and carry on from there. These numbers come from a search over
look-ahead, closing limit and hyphen cost. They should live as named constants in one place.

## 7. Hyphens

**They're only drawn.** A hyphen is an extra glyph at the end of a run, with `start == end` at the break,
so nothing enters the text. Byte offsets, anchors, copying, find and export are untouched.

**Which words.** Lowercase letters only, at least six letters, with at least three before the hyphen and
three after. That leaves out names, acronyms, words with an apostrophe or digit, and anything in a code
span or a link. Hyphenation points come from `hypher`, and a point must fall on a glyph boundary, so a
ligature is never split.

**Its look.** Shape `-` in the style of the glyph before it, cached by style for the call, so an italic
word gets an italic hyphen. It hangs like any other hyphen, at 70% of its width. `hang_line` reads a
glyph's text to decide that, and this glyph has none, so it needs telling.

**The caret needs care.** `caret_line` puts an offset at a break on the next line, but `caret_x` finds the
first run ending there and answers with the end of the line above. With a hyphen the two would disagree,
and the caret would be drawn at the wrong place on the right line. They must agree: the caret goes at the
start of the next line, and tests must pin this, including for selection (`range_rects`), a click on the
hyphen (`offset_at`) and a highlight running across the break.

**Language.** English only, since documents don't record a language. A foreign word in an English essay
can be hyphenated oddly. If that shows up in practice, per-document language is the next step.

**Setting.** `hyphenate` in `KameConfig`, on by default and off through a checkable item in the View menu,
passed down in `LayoutConfig`. `set_config` already drops shaped layouts and textures when that changes.

## 8. Dependencies

The conventions ask for a spec change for a new runtime dependency. This adds two to `kame-typeset`,
both pinned, and neither brings in others:

- `hypher` 0.1.8, with `default-features = false` and only `alloc` and `english`, so one 27 KB table
  rather than all its languages.
- `unicode-linebreak` 0.1.5, which cosmic-text already uses, now also used directly.

## 9. What doesn't change

- **Exports and the site.** Browsers break those lines for themselves, and the stylesheet's test that
  kame doesn't hyphenate stays true. `text-wrap: pretty` and `hyphens: auto` could be tried on the site
  separately.
- **What's stored.** This is all layout, so documents, ops and sync are untouched.
- **Line counts.** Documents can get slightly shorter, since lines hold more on average.

## 10. Testing and building

**Stage 1: setting lines, no hyphens.**
- First fit through the new code matches the engine's breaks, checked with the closing limit and
  look-ahead turned off. This is the check that caught a bug in the study.
- Spaces never go under 90%, except on a line with no other way. A paragraph's lines reach the measure,
  and its last line is natural width.
- Every byte of text is on a line, in order, and `line_start` still works across line breaks.
- An edit doesn't move a line more than one above it, tested over a long paragraph, and laying out twice
  gives identical output.
- Footnote marks, inline code, hanging punctuation, a word too long for the line and an empty paragraph
  all still work.
- Measure layout time on the longest paragraph in a normal debug build, while typing. The study's fits
  took under a millisecond with optimizations on, which a debug build doesn't have.

**Stage 2: hyphens and the setting.**
- The word rules in section 7, the caret and selection cases, and italic and bold hyphens.
- Copying a selection across a hyphen gives the word without one.
- Re-run the numbers with the three-letter rule, which the study didn't use. The essays' seven hyphens all
  pass it, so expect little change there.

**A tool for tuning.** A `cargo run --example` that takes `.kame` files and prints only counts, like
`kame-sync trial`, so the numbers can be checked against new writing. The prototype that produced this
study lives outside the repo and isn't kept.

Each stage is its own commit on one branch, tried in the app before it lands, on both Linux and the Mac.

## 11. Decisions made on your behalf

- **One line ahead, not two.** Two lines is a little better on the essays (8.3% against 12% over 1.5x
  without hyphens) but moved a line above for about 6% of removed words against about 1%. It's one
  constant, so try the other if the page feels too loose.
- **A 90% closing limit,** because hyphens make tighter spacing unnecessary. With hyphens off, 90% leaves
  more loose lines than 80% would; I kept one number rather than two.
- **Hyphens on by default,** with a menu item, and a setting for the app rather than each document.
- **Three letters before a hyphen,** where `hypher` allows two, because the devlog produced be-tween and
  al-ways.
- **Tuned at type 16,** the size in this machine's config. Tell me if you write at another size.

## 12. What this doesn't show

- **The sample is small.** About 130 lines chose these numbers, and one line is nearly 1%. The devlog
  improving too is reassuring, but the numbers may want adjusting with use.
- **How it feels.** The movement figures are counts, and nobody has typed with this yet. Stage 1 is the
  first chance to find out.
- **The picture is only part of it.** Spacing was measured with the study's copy of the rule, on text
  without the room footnote marks take.
