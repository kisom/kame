# Follow-ups

Open work, most pressing first. Tick items off or delete them as they land.

## Publishing

- [ ] **A static site generator.** Spec in
  `docs/superpowers/specs/2026-11-02-kame-site-design.md`. A separate command line tool in
  this workspace, built on `kame-core`, which already has `Db::open_read_only`, `html_body` and its `Publish`
  hooks, and `links`. Decided so far:
  - **Config:** `site.yaml`. `publish` maps a fixed slug to a file, as in
    `commanding-heights: ~/data/writing/CommandingHeights.kame`, so that adding a document
    can never change a published URL. A separate `drafts` list is for early feedback.
  - **URLs:** documents go under `/k/<slug>/`, with their images beside them, as Hugo does.
  - **Output:** into `output/`.
  - **Dates:** `published`, or `created` if there is none.
  - **Document links:** one to a published document goes to its page, and any other
    publishes as its text. A `#Heading` goes to the page, not the heading, and keeping
    links good is the author's job, like web links.
  - **Pages:** Markdown for pages like about and colophon, with Pelican- or Hugo-style
    themes and listings by tag.

## Typography

- [ ] **Word spacing (on hold).** Justified text without hyphenation leaves
  wide gaps. cosmic-text picks each break on its own and only widens spaces.
  Soft hyphens from `hypher` are not enough: on 378 devlog paragraphs at type 18,
  lines with a space over 2x normal fell from 17.1% to 10.3%, but the widest 1% got
  worse, up to 13 hyphenated lines ran in a row, the hyphen is invisible, and the
  inserted characters break the byte-offset invariant. The likely fix is for kame
  to choose its own breaks across the paragraph, with limits on hyphen runs and
  stretch. About 24 call sites read cosmic-text's lines directly, so prototype it
  outside the repo first, then write a spec.

## Markdown import

- [ ] **Tables (deferred).** Simplified on import because the model has no equivalent.
  Tables are wanted but too complex for now, and a code block is the stopgap.

## From earlier feedback

- [ ] **PDF export (on hold).** Still open in `feedback2.txt`; not required for v1.

## Ideas

- [ ] **A kame notebook.** The long-term goal, and notes are in `MultiKame.kame`: an
  extended kame for a binder of kame and Markdown documents, with search across them, quick
  linking and publishing by path.
