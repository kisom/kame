# Follow-ups

Open work, most pressing first. Tick items off or delete them as they land.

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

## History and sync

- [ ] **History coalescing.** Almost all of a `.kame` file is its edit history, one entry per
  keystroke, at about 1 KB per finished word: `CommandingHeights` is 1,255 words and 1.2 MB, of
  which 1.1 MB is history. Two parts, next up:
  - Coalesce as you type, word by word, for text and for note edits, which save the note's whole
    text on every keystroke. Ctrl-Z then undoes a word at a time.
  - A "compact history" command applying the same to existing files. Simulated on
    `CommandingHeights`: 9,535 entries become 1,840, and about 220 KB.
  - Not pruning: every edit that matters is kept.
- [ ] **Syncthing can replace a `.kame` file while it is open.** It did, on 2026-11-02, with
  kame running and a `-wal` beside the file. No text was lost that time, but the `-wal` could have
  held unsaved edits. Until sync exists, close a document before editing it on another machine.

## Ideas

- [ ] **Sync for drift between one person's machines.** A thinking notebook with one user, not a
  Google Docs replacement: mostly a laptop and a desktop that each drifted offline. The op log is
  the natural thing to sync. It could double as a publishing server. Start by detecting divergence
  and merging logs, showing what can't be resolved, with a prototype replaying two diverged logs
  from real documents. To read first: Eg-walker, which keeps plain positional edits as kame does, and
  Peritext, for rich text and annotations. The hard questions are marginalia anchored to a range,
  and what linear undo means after a merge.

- [ ] **A kame notebook.** The long-term goal, and notes are in `MultiKame.kame`: an
  extended kame for a binder of kame and Markdown documents, with search across them, quick
  linking and publishing by path.
