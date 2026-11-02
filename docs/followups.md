# Follow-ups

Open work, most pressing first. Tick items off or delete them as they land.

## Bugs

- [ ] **The status path overprints the toolbar in a narrow window.** The
  right-aligned path collides with the first row's buttons at about 620 points
  wide.

## Typography

- [ ] **Word spacing.** Justified text without hyphenation leaves wide gaps. The
  design spec already lists `hypher` as a deferred dependency.
- [ ] **Footnote superscripts sit tight against the next word** (`source¹and`).
  They render in the word space rather than taking room of their own.
- [ ] **Annotation washes look like pasted patches**, such as the white box
  around an anchored word.
- [ ] **Toolbar weight.** Three rows compete with the page, and the monospace path
  clashes with the Garamond.
- [ ] **Finer spacing.** Space around headings and quotes, and hanging punctuation
  in the margins.
- [ ] **Antialiasing check.** On a Mac, drag the window to a display with a
  different scale. Only tests have covered text re-rendering at the new scale.

## Markdown import

- [ ] **Exercise the UI by hand.** The Import Markdown button, its file picker and
  the discard-changes dialog have not been clicked through, on Linux or macOS.
- [ ] **Site-root images leave blank gaps.** `/files/images/...` cannot be found
  from the file alone, and kame reserves blank space for an image it cannot load.
  Options: ask for a site root at import, or collapse the space.
- [ ] **First save suggests `Untitled.kame`** after an import. It should suggest
  `<stem>.kame`.
- [ ] **Line breaks inside a block do not survive export.** Code and hard breaks
  reflow when exported and re-imported, because the exporter writes a bare
  newline. The corpus test skips export stability for those documents.
- [ ] **Code has no typeface of its own.** It imports as plain prose, keeping its
  line breaks and indentation but not a monospaced face.
- [ ] **Decide on smart punctuation.** `---` and straight quotes import as typed,
  though posts may expect em dashes and curly quotes. Converting them on import
  would break the export round trip.

## From earlier feedback

- [ ] **PDF export.** Still open in `feedback2.txt`; not required for v1.
