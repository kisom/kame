# Keyboard

The editing map lives in `command_for_key` (`src/render/egui_render.rs`); the
view-mode map lives in `mode_for_key` (`src/main.rs`), and the file/export map
in `file_command_for_key` (also `src/main.rs`). `Ctrl` below means `Cmd` on
macOS (`Modifiers::command`).

| Key | Action |
| --- | --- |
| text, Enter, Backspace, Delete | insert, new paragraph, delete back/forward |
| Ctrl-Z / Ctrl-Shift-Z / Ctrl-Y | undo / redo / redo |
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

## Typed markdown

Formatting is also applied as you type, at the start of a line or around a
word — each one is a single undo step:

- `# `, `## `, `### ` at the start of a line → a heading
- `- `, `* `, or `1. ` at the start of a line → a list item
- `> ` at the start of a line → a block quote
- `*x*` / `**x**` around a word → italic / bold
- `[text](url)` → a hyperlink on `text`

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

Opening a `.md` or `.markdown` file imports it: with Ctrl-O, File > Import
Markdown, or `kame notes.md`. The file is only read, and the result is a new
document with no file, so the first save asks where to put it. Front matter
names the document and becomes its opening heading. Code, tables and raw HTML
become plain text, nested lists flatten, and rules and image descriptions are
dropped; the status line reports what was simplified and any images it could
not find. Replacing a document that has no file, or whose last save failed,
asks first.

The exports write beside the document you opened: `<stem>.md` and `<stem>.html`
in the same directory as the `.kame` file (in the working directory when the
path has no directory part). They read the document as it stands, so nothing
has to be saved first, and a failed write says so in the status panel without
making the document read-only.

A snapshot is a complete copy of the document stored in the `.kame` file,
outside the undo history. Restoring one is the only command here that changes
the text, and it is deliberately **not** an undo step:

- it asks for confirmation first, because the current text is discarded;
- the restored text starts a fresh history, so the snapshot itself is the undo
  point — take one before a risky revision if you may want to come back;
- the snapshots are kept, so restoring a newer one is how a restore is undone.

| Key | Action | Effect |
| --- | --- | --- |
| Ctrl-O | open | prompt for a path, replacing the current document |
| Ctrl-S | save | a document already on disk is saved continuously, so this is a no-op; an anonymous document opens the system save-as browser |
| Ctrl-Shift-E | export Markdown | writes `<stem>.md` beside the `.kame` file |
| Ctrl-Shift-B | export HTML | writes `<stem>.html`, a standalone page |
| Ctrl-Shift-S | snapshot now | prompts for an optional label, then stores the document |
| Ctrl-Shift-R | restore snapshot | lists the snapshots (newest first); choose one, then confirm |

Both dialogs take Enter as their primary action and Escape as cancel. Nothing
auto-repeats: one press is one command, and a held Enter cannot step through
the restore confirmation.

Clicking a footnote's number drops down to the footnote.

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
