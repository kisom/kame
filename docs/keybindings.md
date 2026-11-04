# Keyboard

The editing map lives in `command_for_key` (`crates/kame-typeset/src/render/egui_render.rs`); the
view-mode map lives in `mode_for_key` (`src/main.rs`), and the file/export map
in `file_command_for_key` (also `src/main.rs`). `Ctrl` below means `Cmd` on
macOS (`Modifiers::command`).

| Key | Action |
| --- | --- |
| text, Enter, Backspace, Delete | insert, new paragraph, delete back/forward |
| Ctrl-Z / Ctrl-Shift-Z / Ctrl-Y | undo / redo / redo, a word at a time as you type |
| Ctrl-C / Ctrl-X / Ctrl-V | copy / cut / paste, as plain text |
| Shift-Enter | in a code or prose block, a line without leaving the block |
| Ctrl-Shift-1 / -2 / -3 | make the caret's block a level 1/2/3 heading |
| Ctrl-Shift-0 | return the caret's block to body text |
| Ctrl-Shift-N / Ctrl-Shift-F | add a note (a highlight) / a footnote at the selection or the word under the caret. A footnote's selection grows to whole words |
| Ctrl-K | add or edit a hyperlink on the selection (an empty URL removes it) |
| Arrows, Home, End | move the caret (up/down cross into the adjacent block at the edge) |
| Page Up, Page Down | move the caret and the page a window at a time, or to the very start or end from the first or last line; with Shift, extend the selection |
| Ctrl-Left / Ctrl-Right | move the caret a whole word at a time |
| Shift-wheel or a sideways swipe | scroll a code block whose lines run past the column; the caret also brings its block along, and a solid edge marks each side with more to see |
| wheel or trackpad | scroll the page, bringing the caret along when it would leave the page, unless text is selected or typewriter mode is on |
| Ctrl-Shift-Left / Ctrl-Shift-Right | extend the selection a whole word at a time |
| Ctrl-A | select the caret's whole block (again for the whole document) |
| Ctrl-F | find — open the find bar |
| Ctrl + / Ctrl - / Ctrl-0 | increase / decrease / reset the type size |
| drag, shift-click, Shift-arrows | select / extend the selection |
| Escape | close the find bar, a dialog, or the note popover |
| F1 | toggle the help dialog |

## Selecting

Double-click a word to select it, and keep the button down on the second click to drag out by
whole words. Drag or Shift-click to select a range by character.

## Moving and deleting by word

| Key | Action |
| --- | --- |
| Ctrl-Left/Right or Alt-Left/Right | move by word; add Shift to select |
| Alt-Backspace or Esc then Backspace | delete the word before the caret |
| Esc then `<` / Esc then `>` | go to the very start / the very end of the document |

Esc counts as a prefix when it has nothing to close, and for two seconds.

## Nested lists

In a list item, Tab moves it a level in and Shift-Tab a level out, and with several items
selected they all move together. An item can only go one level below the item before it,
so a list's first item stays put. An item's children stay where they are when it moves.
Numbers nest as `1.`, `1.1`, `1.1.1`, on the page and in HTML. Markdown has no way to write
that, so it exports plain numbers and leaves the numbering to whatever renders it. Enter on an empty nested
item comes out a level, and on an empty top-level item returns to body text.

## Typed markdown

Formatting is also applied as you type, at the start of a line or around a
word — each one is a single undo step:

- `# `, `## `, `### ` at the start of a line → a heading
- `- `, `* `, or `1. ` at the start of a line → a list item
- `` `code` `` anywhere → inline code, in IBM Plex Mono, with what is inside it literal
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
type. Enter finds the next match, Shift-Enter the previous one, and Escape or
clicking back in the text closes the bar; the page scrolls to keep the current
match in view.

## Block kinds

Block kind is also on the page's right-click menu — body, H1–H3, both list kinds,
block quote, code and prose — with a check beside the caret's current kind. Changing
the kind is one undoable edit, and undo restores the previous kind (including a
heading's level).

The digit chords are matched against the physical digit as well as the typed
character: `Shift`+1 gives `!`, which egui names `Exclamationmark`, and a
layout where `Shift`+1 is not `!` at all still reaches H1 through the digit's
position on the keyboard.

## Notes, marginals and footnotes

Ctrl-Shift-N adds a note and Ctrl-Shift-F a footnote, at the selection or the word under the
caret. A marginal belongs to a whole block and has no key: all three are on the Annotate menu and
the right-click menu. A note needs no words, in which case it's just a highlight.

Each opens for writing as it's added. Ctrl-Enter, Close or Escape closes it, and Remove takes it
out, along with its highlight. Emptying a marginal removes it too.

## Images

Insert image… on the right-click menu asks for a path, and the image is set to the width of the
text.

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

View > Check spelling marks likely mistakes as you write, in red for spelling and blue for anything else, without
changing a word. Right-click a mark to take a suggestion, accept it for this document, add the word to this document's
list or your own, or turn off the rule. F7 selects the next mark in the text. Margin notes and footnotes are checked too,
though F7 doesn't go to them. Code, links, quotes, prose blocks and notes aren't checked.

File > Document Properties shows this document's words and accepted marks, to change or take back. Your own words and the rules
you've turned off are in `config.json`, as `words` and `rules_off`.

View > Hyphenate turns off the hyphens kame draws to even out justified paragraphs. They are only
drawn, so they never reach your text, copies or exports.

Dark mode (`Ctrl-Shift-D`) is separate from these: a persisted preference
(saved to the config and surviving a restart), not a transient view mode.

## Files, exports and snapshots

On macOS, `kame.app` opens `.kame` files from Finder, and can open Markdown files
through Open With. Quitting by any route, from Cmd-Q to the Dock, goes through the
prompt for an unsaved document.

Opening a `.md` or `.markdown` file imports it: with Ctrl-O, File > Import
Markdown, or `kame notes.md`. The file is only read, and the result is a new
document with no file, so the first save asks where to put it. Front matter
names the document and becomes its opening heading. Tables and raw HTML become
plain text, and image descriptions are dropped; the status line reports what was simplified and any images it could
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

Clicking a footnote's number drops down to the footnote, and clicking the number at the start of the footnote goes
back to its place in the text. Clicking the footnote's words opens it for editing.

## Coming back

Launching kame without naming a file reopens the last file you had open, if it is
still there. After Ctrl-N, or after importing Markdown, it starts empty instead, until
that document is saved or another file is opened. A second kame started while one is running begins empty instead, because two
windows on one file would overwrite each other's edits.

Each document keeps its caret and scroll position in its own file, and returns to them
whenever it is opened.

## Links between documents

File > Link to Document…, or the right-click menu, asks for a file and links to it, as
`~/path/to/essay.kame` when it is under your home folder. With text selected, the selected
words become the link. With the caret in a link, the link points at the new file instead.
Otherwise the document's title goes in at the caret as the link's text.

Or type `[[essay]]`, `[[notes/essay#The Intro]]` or `[[notes/essay#The Intro|the intro]]`. The
text after `|` is what shows, or the target if there is none. A link entered as an
ordinary link, with Ctrl-K or `[text](url)`, is a document link too if it names a `.kame`
file or starts with `#`. Document links are underlined, and clicking one opens the
document in kame.

The target is kept exactly as you entered it, and a relative path starts from the folder
of the document holding the link. An extension can be left off: `.kame` and then `.md` are
tried. `#The Intro` goes to the first heading with exactly that text, and on its own it
means a heading in this document. If a link has rotted, the status row says so and nothing
opens.

Ctrl-[, or File > Back, returns to where you followed the link from, as many links back as you
went.

## The default author

A new document starts with the author you have chosen, as does an import that doesn't name one.
Set it with "Use this author for new documents" in Document Properties, or as `"author"` in kame's
`config.json`. Documents that already exist keep what they have. On a site, a document with no
author takes `author` from `site.yaml`.

## History

Everything you type is kept in the document's history, joined up a word at a time, so that undo
takes back a word. File > Compact History does the same for what was saved before kame did this,
which makes the file much smaller. Nothing you wrote is lost, only the stopping points inside words.

## What exports publish

Code exports as a fence and as `<pre><code>`, keeping its language. Prose exports to
HTML exactly, and to Markdown as a block quote with hard line breaks, which keeps its
lines but not extra spacing.

A link to a kame document publishes as its text, since it can't lead anywhere outside
kame.

HTML shows the author under the first heading, and the passages your notes highlight, though
never the notes' words. Markdown has no highlight, so those passages are plain text there. HTML
uses kame's own type and colours, and follows the reader's system between light and
dark. Footnotes publish in both formats and marginalia in HTML. Markdown has no form for a
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
mode, hyphenation, spelling, a notebook's commands and the view modes) and Help.

On Linux, Alt-F, Alt-A, Alt-V and Alt-H open those menus; arrows and Enter then
choose an item, and Escape closes it. There are no such chords on macOS, where
Option plus a letter types a character. The document takes no keystrokes while a
menu is open.

The status row below it shows the document's title (hover for its path), `not
saved yet` if it has no file, any mode badges, and the word count.

A notice shows in the row, cut to fit with the full text on hover, until the next edit or undo. An error stays until
it is resolved.

## Other versions of a document

If a sync tool leaves a conflict copy beside the open document, the status row says so and File >
Merge Conflict Copies merges it. Sentences both versions changed keep the words on screen, with the
other version in a note. A merge is one undo, after a snapshot, and the copy stays beside the document
with `.merged` added to its name.

If the file itself is replaced while open, kame stops saving to it and asks whether to merge the two,
save this version under a new name, or open the one on disk.

## Notebooks

File > New Notebook… makes a folder you choose into a notebook, keeping anything already in it, and opens its new `Notebook.kame`. So does
`kame site new-notebook FOLDER`, which then needs opening.

A folder with a `notebook.yaml` in it is a notebook, and opening any document under it is opening the notebook. `kame FOLDER` opens its
`Notebook.kame`, or the browser if it has none. The file holds `title`, `url`, `author` and `remote`, all optional, and an empty one
is fine. These do something only in a notebook, and are also in the View menu:

| Key | Action |
| --- | --- |
| Ctrl-L | jump to a document by title or path. Letters only have to appear in order. If nothing matches, the last row offers to create it |
| Ctrl-Shift-L | go to `Notebook.kame`, the contents page |
| Ctrl-R | search every document, including margin notes, footnotes and private notes, matching as Ctrl-F does. Every match is listed |
| Ctrl-Shift-O | the browser, a folder tree on the left. If the text and its margin don't fit beside it, the page hides until you choose something or close it |

Both lists take the arrows, Enter and Escape. The browser and the jump list notice what arrives from another machine within about 10 seconds of Syncthing delivering it. Ctrl-[ comes back from anywhere they lead.

Following a link to a document that isn't there creates it, inside the notebook only, as a `.kame` with any folders it needs. A heading in
the link becomes its first heading, and a link to a folder makes that folder's `Notebook.kame`. Leave it unwritten and it's removed again.
A link to any other kind of file opens it in the system's viewer, and a missing one says so.

Below the footnotes, "Linked from" lists the documents linking to this one. It's only drawn, and never reaches an export or a site.

What's searched is what this machine has. With revision logs, a closed document can be behind what another machine wrote until it's next
opened.

To move or delete a document in a folder with revision logs, move or delete both `X.kame` and `.X.kame.sync/`. Moving one leaves a stale copy
on the other machine for now.

### The site tool

`kame site ...` is `kame-site ...`, with no window, so the app is all you need to have. Run under the name `kame-site` it behaves as that
too, so on a Mac a link keeps the two in step:

```sh
ln -sf ~/Applications/kame.app/Contents/MacOS/kame ~/.local/bin/kame-site
```

A file named `site` opens as `kame ./site`.

### Publishing a notebook

`kame-site build FOLDER` builds it as its own site, in an `output/` folder in the notebook, which the browser and search leave out. In a
synced folder, stop that syncing with `/path/to/notebook/output` and `/path/to/notebook/.output-*` in `.stignore`, or two machines' builds
will conflict. A build won't replace an `output/` it didn't make.

`kame-site sync FOLDER` also sends it to `remote` with `rsync --delete`. A dry run comes first, and if that would delete more than 5 files
from the remote, they're listed and nothing happens without `--yes`. Document Properties has a *Publish* tick, and in Markdown it's `publish: true` in the front matter. `Notebook.kame`
in the root is the home page and always publishes.

URLs follow the folders, so `maritain/materialism.kame` is `/maritain/materialism/`, and a marked `Notebook.kame` in a folder is that
folder's page. Everything in `static/` publishes at `/static/`, whether or not anything links to it, and isn't searched. Anything not for
publication goes in another folder. A link to an unmarked document, or to a file outside `static/`, publishes as its words, and the build
lists it. A link to something missing under `static/` or to a folder publishes as written, for the web server to answer, and any other is its words. An image shown in a published
document is copied beside its page, unless it's outside the notebook, which the build also lists.

Insert image… on the right-click menu asks for a file, starting in `static/`, and shows it at its own size, or at the body's width if it's wider. An image inside the notebook is kept as the way there from the document's folder, so it
still shows on another machine and after the notebook moves. One outside keeps its whole path. Saving a document somewhere else says its images' paths again from there, and an export
uses whole paths, since it can land anywhere. A document whose images already have whole paths keeps them until it's saved somewhere else.

The theme is the standard one, and a file in the notebook's `theme/` takes the place of the same file in it. With no `url` there's no feed.

## Revision logs

This is a trial, for a folder at a time. `kame-sync enable FOLDER` puts a `.kame-logs` file in a folder, and prints a line that every
machine's `.stignore` needs. Documents under that folder then stop syncing their databases, and each machine keeps its own log of
revisions in `.X.kame.sync/`, which is what syncs.

kame writes a revision on a pause in typing, on losing focus and on leaving a document. What arrives from another machine shows up on
a pause or on coming back to the window, never mid-word, and the status row says where it came from. A copy left open follows along,
with everything that arrives between your own edits as one undo step. Sentences both machines changed keep the later
version in the text and the other in a note, after a snapshot. A document with only logs, as on a new machine, is built when opened.

Snapshots and undo history stay on the machine that made them. To go back, remove the `.stignore` line, then delete `.kame-logs` and the
hidden `.sync` folders.
