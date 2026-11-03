# Follow-ups

Open work, most pressing first. Tick items off or delete them as they land.

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

- [ ] **Richer Markdown elements (deferred).** Tables, nested lists and inline code
  are simplified on import because the model has no equivalent.

## Linking

- [ ] **Links to other kame documents.** Link from one document to another, and open it
  in kame on a click rather than handing it to the system. Still to decide: how such a
  link is stored so that it survives moving a folder of documents, whether it can point
  at a place inside the document, and what it becomes on export. That last one depends
  on the static site generator.

## From earlier feedback

- [ ] **PDF export.** Still open in `feedback2.txt`; not required for v1.
