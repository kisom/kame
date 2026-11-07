# Changelog

What changed in each release of kame, newest first. A release's section is also its release notes on GitHub.

## [0.2.0] - 2026-11-06

The first release that knows how to meet a newer one: from here on, a document or a revision log written by a later
kame is kept safe and shown read-only, never refused or damaged. The file format is frozen as version 1, and every
later build is checked against what this one writes.

### Added

- `kame --version` and `kame-site --version` (and `-V`) say which version is running, and so does the top of Help.
- An Edit menu, after File: Undo and Redo, Cut, Copy, Paste and Select All, Find, the headings and Body Text, Link…,
  and moving, indenting and outdenting list items. Each row runs what its key does, and is greyed out, saying why,
  when it can't act. Alt-E opens it, and the command palette lists its commands.
- Ctrl-Shift-Up and Ctrl-Shift-Down (Cmd on macOS) move the caret's list item, or the selected items, up or down
  among the items of their list, with the items nested under them. A move is one undo step.
- The interface's text size can be changed apart from the type size: Ctrl-Alt-= and Ctrl-Alt--, Ctrl-Alt-0 to
  reset, or the View menu. It covers the menus, the title and status line, the notebook browser, the palette, help
  and dialogs, and is kept between launches.
- A Markdown table imports as a code block marked `markdown`, holding the table exactly as it was written, ready to
  become a table once kame has them. Footnotes that only a table cites become one marginal on it.
- A sync that merges in another document of the same name, such as two machines making the same document from one
  link, says so: "merged with another document of this name from MACHINE".
- The release workflow runs the tests, wants the version's sample files, checks the installed app's `--version`,
  and installs the app and `kame-site` from the Homebrew tap on a clean Mac.

### Changed

- A document holding a kind, style or field that a newer kame wrote opens read-only, showing what this version can
  and saying to update, where before it was refused as corrupt or opened with the field dropped. Nothing is written
  to it or its logs from such a view; exports, copies and site builds leave out what this version can't show, and a
  site build warns. Moves, renames, reference renames, conflict-copy merges and the stranded sweep leave such a
  document alone, and say why.
- Ctrl-= (Cmd-= on macOS) makes the type larger, as Ctrl-+ does. The type size keys no longer also zoom the whole
  window.
- While a document can't be changed (its last save failed, its file changed on disk, or another kame has it), Ctrl-X
  and Ctrl-K are refused as every other change is, so a cut never only copies. Copy still works.
- List bullets are drawn as shapes of one size at every level; the third level's square no longer comes from an
  emoji font.

### Fixed

- A field from a newer kame inside a step of history is noticed, so undo or Save As can't lose it.
- A document whose revision logs only a newer kame can read is refused with the update message, instead of being
  made again empty.
- Menus taller than the window scroll; the notebook's lists open below the status row at any interface text size.
- Ctrl-Alt-Shift-0 no longer also makes the caret's block body text.

## [0.1.2] - 2026-11-06

- What a kame writes into documents it doesn't have open now respects the lock another kame holds: a move, rename or
  delete of a document open elsewhere, or of one linking to it, is refused, naming it; a reference rename skips it
  and keeps the old reference; the stranded sweep leaves it for a later pass; a conflict copy open elsewhere isn't
  merged.

## [0.1.1] - 2026-11-06

- The release builds the macOS app on GitHub's current runners.

## [0.1.0] - 2026-11-06

- The first release: kame for macOS through the Homebrew tap and GitHub, and for Arch through the AUR, under the
  Apache-2.0 license, with the embedded fonts under the OFL.

[0.2.0]: https://github.com/kisom/kame/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/kisom/kame/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/kisom/kame/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/kisom/kame/releases/tag/v0.1.0
