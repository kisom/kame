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

- [ ] **A static site generator for blog posts.** Working from `.kame` files, and the
  occasional Markdown file, so marginals can be laid out properly. Markdown export
  drops them. It would also settle site-root images, like `/files/images/...`, which
  cannot be found from the file alone and show the missing-image placeholder.

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

## Markdown import

- [ ] **Richer Markdown elements (deferred).** Code, tables and nested lists are
  simplified on import because the model has no equivalent. Code keeps its line
  breaks and indentation but not a monospaced face. Supporting them is significant
  work and is not needed yet.

## From earlier feedback

- [ ] **PDF export.** Still open in `feedback2.txt`; not required for v1.
