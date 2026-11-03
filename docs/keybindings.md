# Keyboard

The editing map lives in `command_for_key` (`src/render/egui_render.rs`); the
view-mode map lives in `mode_for_key` (`src/main.rs`), and the file/export map
in `file_command_for_key` (also `src/main.rs`). `Ctrl` below means `Cmd` on
macOS (`Modifiers::command`).

| Key | Action |
| --- | --- |
| text, Enter, Backspace, Delete | insert, new paragraph, delete back/forward |
| Ctrl-Z / Ctrl-Shift-Z / Ctrl-Y | undo / redo / redo |
| Shift-Enter | in a code or prose block, a line without leaving the block |
| Ctrl-Shift-1 / -2 / -3 | make the caret's block a level 1/2/3 heading |
| Ctrl-Shift-0 | return the caret's block to body text |
| Ctrl-Shift-N / Ctrl-Shift-F | add a note (a highlight) / a footnote at the selection or the word under the caret |
| Ctrl-K | add or edit a hyperlink on the selection (an empty URL removes it) |
| Arrows, Home, End | move the caret (up/down cross into the adjacent block at the edge) |
| Ctrl-Left / Ctrl-Right | move the caret a whole word at a time |
| Ctrl-Shift-Left / Ctrl-Shift-Right | extend the selection a whole word at a time |
| Ctrl-A | select the caret's whole block (again for the whole document) |
| Ctrl-F | find — open the find bar |
| Ctrl + / Ctrl - / Ctrl-0 | increase / decrease / reset the type size |
| drag, shift-click, Shift-arrows | select / extend the selection |
| Escape | close the find bar, a dialog, or the note popover |
| F1 | toggle the help dialog |

## Moving and deleting by word

| Key | Action |
| --- | --- |
| Ctrl-Left/Right or Alt-Left/Right | move by word; add Shift to select |
| Alt-Backspace or Esc then Backspace | delete the word before the caret |
| Esc then `<` / Esc then `>` | go to the very start / the very end of the document |

Esc counts as a prefix when it has nothing to close, and for two seconds.

## Typed markdown

Formatting is also applied as you type, at the start of a line or around a
word — each one is a single undo step:

- `# `, `## `, `### ` at the start of a line → a heading
- `- `, `* `, or `1. ` at the start of a line → a list item
- `> ` at the start of a line → a block quote
- `| ` at the start of a line → a prose block
- ` ``` ` or ` ```rust ` then Enter → a code block
- `---` then Enter → a horizontal rule
- `*x*` / `**x**` around a word → italic / bold
- `[text](url)` → a hyperlink on `text`

A code or prose block keeps its text exactly: line breaks and spacing stand, Tab types
a tab, and pasted lines stay in the block. Enter adds a line, and Enter on a blank last
line returns to body text. Shift-Enter adds a line without ever leaving, for blank lines
inside the block. Prose is set in the body face, code in IBM Plex Mono.

Tab does nothing in any other block. It moves between fields only in a dialog.

Typed bold and italic work in prose. In code they stay literal, and typed links stay
literal in both.

## Search

Ctrl-F opens the find bar. Matches are case-insensitive and highlight as you
type. Enter finds the next match, Shift-Enter the previous one, and Escape
closes the bar; the page scrolls to keep the current match in view.

Block kind is also on the page's right-click menu — body, H1–H3 and both list
kinds. Changing the kind is one undoable edit, and undo restores the previous
kind (including a heading's level).

The digit chords are matched against the physical digit as well as the typed
character: `Shift`+1 gives `!`, which egui names `Exclamationmark`, and a
layout where `Shift`+1 is not `!` at all still reaches H1 through the digit's
position on the keyboard.

## Distraction-free modes

Every mode is view-only UI state: never persisted, never an undo step, and
typing is unaffected while any of them is on. The status row shows a badge for
each active mode: `FS` fullscreen, `M` margin hidden, `F` paragraph focus and `T`
typewriter. Hovering one names it.

| Key | Mode | Effect |
| --- | --- | --- |
| F11 | fullscreen | toggles the window fullscreen |
| Ctrl-Shift-H | hide chrome | hides the menu bar and the status row |
| Ctrl-Shift-M | hide margin | collapses the margin gutter; marginal notes and note markers are not drawn (their anchored ranges stay marked, footnotes stay at the foot) |
| Ctrl-Shift-P | paragraph focus | dims every block but the one holding the caret |
| Ctrl-Shift-T | typewriter | keeps the caret's line at the vertical centre of the window; the page re-centres when the caret moves and can still be scrolled while idle |

Held keys do not auto-repeat a toggle: one press is one flip.

Dark mode (`Ctrl-Shift-D`) is separate from these: a persisted preference
(saved to the config and surviving a restart), not a transient view mode.

## Files, exports and snapshots

On macOS, `kame.app` opens `.kame` files from Finder, and can open Markdown files
through Open With. Quitting by any route, from Cmd-Q to the Dock, goes through the
prompt for an unsaved document.

Opening a `.md` or `.markdown` file imports it: with Ctrl-O, File > Import
Markdown, or `kame notes.md`. The file is only read, and the result is a new
document with no file, so the first save asks where to put it. Front matter
names the document and becomes its opening heading. Tables, inline code and raw
HTML become plain text, nested lists flatten, and image descriptions are dropped; the status line reports what was simplified and any images it could
not find. Replacing a document that has no file, or whose last save failed,
asks first.

Each export asks where to write in the system's save dialog, suggesting
`<stem>.md` or `<stem>.html` beside the document, or beside an unsaved import's
source. Cancelling writes nothing. Exports read the document as it stands, so
nothing has to be saved first, and a failed write says so in the status panel
without making the document read-only.

Open and Save As use the system's file browser. File > Open Path takes a typed path
instead, with `~` expanded and relative paths resolved beside the current document.
Save As carries the history, snapshots and properties to the new file and continues
there, leaving the old file as it was. Saving over an existing file replaces it
whole.

A snapshot is a complete copy of the document stored in the `.kame` file,
outside the undo history. Restoring one is the only command here that changes
the text, and it is deliberately **not** an undo step:

- it asks for confirmation first, because the current text is discarded;
- the restored text starts a fresh history, so the snapshot itself is the undo
  point — take one before a risky revision if you may want to come back;
- the snapshots are kept, so restoring a newer one is how a restore is undone.

| Key | Action | Effect |
| --- | --- | --- |
| Ctrl-N | new | an empty document, asking first if the current one has unsaved work |
| Ctrl-O | open | the system's file browser, replacing the current document |
| Ctrl-S | save | a document already on disk is saved continuously, so this is a no-op; an anonymous document opens the system save-as browser |
| Ctrl-Shift-E | export Markdown | asks where, suggesting `<stem>.md` |
| Ctrl-Shift-B | export HTML | asks where, suggesting `<stem>.html`, a standalone page |
| Ctrl-Shift-S | snapshot now | prompts for an optional label, then stores the document |
| Ctrl-Shift-R | restore snapshot | lists the snapshots (newest first); choose one, then confirm |

Both dialogs take Enter as their primary action and Escape as cancel. Nothing
auto-repeats: one press is one command, and a held Enter cannot step through
the restore confirmation.

Clicking a footnote's number drops down to the footnote.

## Coming back

Launching kame without naming a file reopens the last file you had open, if it is
still there. After Ctrl-N, or after importing Markdown, it starts empty instead, until
that document is saved or another file is opened. A second kame started while one is running begins empty instead, because two
windows on one file would overwrite each other's edits.

Each document keeps its caret and scroll position in its own file, and returns to them
whenever it is opened.

## Links between documents

Type `[[essay]]`, `[[notes/essay#The Intro]]` or `[[notes/essay#The Intro|the intro]]`. The
text after `|` is what shows, or the target if there is none. A link entered as an
ordinary link, with Ctrl-K or `[text](url)`, is a document link too if it names a `.kame`
file or starts with `#`. Document links are underlined, and clicking one opens the
document in kame.

The target is kept exactly as you entered it, and a relative path starts from the folder
of the document holding the link. An extension can be left off: `.kame` and then `.md` are
tried. `#The Intro` goes to the first heading with exactly that text, and on its own it
means a heading in this document. If a link has rotted, the status row says so and nothing
opens.

## What exports publish

Code exports as a fence and as `<pre><code>`, keeping its language. Prose exports to
HTML exactly, and to Markdown as a block quote with hard line breaks, which keeps its
lines but not extra spacing.

A link to a kame document publishes as its text, since it can't lead anywhere outside
kame.

Footnotes publish in both formats and marginalia in HTML. Markdown has no form for a
marginal, so they stay out of it. Notes are private and never publish, in either
format.

## Document properties

File > Document Properties holds the document's title, author, created and
published dates, and tags. They are stored in the `.kame` file apart from its
text: saving them is not an undo step, and restoring a snapshot leaves them alone.
A document with no title goes by its file's name.

They come from an import's front matter, where `Date` is the published date. Other
front matter fields are left out, and the status line counts them.

A `javascript:` link keeps its words and loses its target when imported or
exported, and clicking one does nothing.

Exports publish them. Markdown opens with a `Title:`, `Date:`, `Author:` and
`Tags:` header, in the form Pelican reads, leaving out empty fields, and drops a
first heading repeating the title, since a blog renders the title itself. HTML puts
them in the page's head. The date is the published date, or the created date until
there is one.

## The menu bar and status row

The menu bar holds every command with its shortcut: File (open, save, import,
export, snapshots, document properties, and the document's counts), Annotate, View (type size, dark
mode and the view modes) and Help.

On Linux, Alt-F, Alt-A, Alt-V and Alt-H open those menus; arrows and Enter then
choose an item, and Escape closes it. There are no such chords on macOS, where
Option plus a letter types a character. The document takes no keystrokes while a
menu is open.

The status row below it shows the document's title (hover for its path), `not
saved yet` if it has no file, any mode badges, and the word count.

A notice stays under the row until the next edit or undo. An error stays until
it is resolved.

## Build

`cargo` is not on the default PATH on this machine; build in a nix shell:

```sh
nix-shell -p cargo rustc gcc pkg-config libxkbcommon libx11 vulkan-loader wayland \
  --run 'cargo build'
```

Headless run (no display), with Xvfb and the software Vulkan driver:

```sh
Xvfb :99 -screen 0 1400x1000x24 &
DISPLAY=:99 VK_ICD_FILENAMES=/run/current-system/sw/share/vulkan/icd.d/lvp_icd.x86_64.json \
  ./target/debug/kame /tmp/doc.kame
```
