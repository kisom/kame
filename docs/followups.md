# Follow-ups

Open work, most pressing first. Tick items off or delete them as they land.

## Bugs

- [ ] **Quit from the Dock skips the unsaved-document prompt on macOS.** It reaches
  AppKit's `terminate:` with no window close request. Catching it needs an
  application delegate. Cmd-Q goes through the prompt, at the cost of the default
  menu's Hide items, which could come back with a menu of kame's own.

- [ ] **Double-clicking a `.kame` file on macOS.** `Info.plist` declares no document
  types, and Finder sends the file by Apple event rather than as an argument, so
  kame needs code to receive it.

## Publishing

- [ ] **How marginals should publish.** Undecided. Markdown has no form for them, so
  for now they export as raw `<aside class="marginalia">` after their block, which
  keeps the text but needs a stylesheet to read as a marginal. A static site
  generator working from `.kame` files, and the occasional Markdown file, may be the
  better answer, since it could lay them out properly. Blog posts are the goal.

## Typography

- [ ] **Word spacing (low priority).** Justified text without hyphenation leaves
  wide gaps. cosmic-text picks each break on its own and only widens spaces.
  Soft hyphens from `hypher` are not enough: on 378 devlog paragraphs at type 18,
  lines with a space over 2x normal fell from 17.1% to 10.3%, but the widest 1% got
  worse, up to 13 hyphenated lines ran in a row, the hyphen is invisible, and the
  inserted characters break the byte-offset invariant. The likely fix is for kame
  to choose its own breaks across the paragraph, with limits on hyphen runs and
  stretch. About 24 call sites read cosmic-text's lines directly, so prototype it
  outside the repo first, then write a spec.
- [ ] **Hanging punctuation in the margins.** Not started.
- [ ] **Selection colour.** `selection_fill` is built like the washes were, with
  channels above alpha, so a selection lightens the page rather than tinting it.

## Markdown import

- [ ] **A linked image loses its link.** `[![](a.png)](b.png)` imports as just the
  image, because image blocks have no link.
- [ ] **Site-root images show as missing.** `/files/images/...` cannot be found
  from the file alone, so they get the missing-image placeholder. Asking for a
  site root at import would resolve them.
- [ ] **Richer Markdown elements (deferred).** Code, tables and nested lists are
  simplified on import because the model has no equivalent. Code keeps its line
  breaks and indentation but not a monospaced face. Supporting them is significant
  work and is not needed yet.
- [ ] **Decide on smart punctuation.** `---` and straight quotes import as typed,
  though posts may expect em dashes and curly quotes. Converting them on import
  would break the export round trip.

## From earlier feedback

- [ ] **PDF export.** Still open in `feedback2.txt`; not required for v1.
