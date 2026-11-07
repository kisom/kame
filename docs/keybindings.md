# Keyboard

The editing map lives in `command_for_key` (`crates/kame-typeset/src/render/egui_render.rs`); the
view-mode map lives in `mode_for_key` (`src/main.rs`), and the file/export map
in `file_command_for_key` (also `src/main.rs`). `Ctrl` below means `Cmd` on
macOS (`Modifiers::command`).

| Key | Action |
| --- | --- |
| text, Enter, Backspace, Delete | insert, new paragraph, delete back/forward |
| Ctrl-Z / Ctrl-Shift-Z / Ctrl-Y | undo / redo / redo, a word at a time as you type |
| Ctrl-C / Ctrl-X / Ctrl-V | copy / cut / paste. Within kame, formatting, links, notes, footnotes and marginals go too. A paste is one undo step |
| Shift-Enter | in a code or prose block, a line without leaving the block |
| Ctrl-Shift-1 / -2 / -3 | make the caret's block a level 1/2/3 heading |
| Ctrl-Shift-0 | return the caret's block to body text |
| Ctrl-Shift-N / Ctrl-Shift-F | add a note (a highlight) / a footnote at the selection or the word under the caret. A footnote's selection grows to whole words |
| Ctrl-Shift-M | add a marginal on the caret's block, or on the first block of a selection, showing the margin if it's hidden |
| Ctrl-Shift-C | cite a reference, in a notebook. In or just after a `[key]` with no reference it opens a new one with that key, in or just after a known one it opens that reference for editing, and elsewhere it searches the references and inserts `[key]` |
| Ctrl-K | add or edit a hyperlink on the selection, or the link under the caret: its words and its URL (empty words show the URL, and an empty URL removes the link) |
| Arrows, Home, End | move the caret (up/down cross into the adjacent block at the edge) |
| Page Up, Page Down | move the caret and the page a window at a time, or to the very start or end from the first or last line; with Shift, extend the selection |
| Ctrl-Left / Ctrl-Right | move the caret a whole word at a time |
| Shift-wheel or a sideways swipe | scroll a code block whose lines run past the column; the caret also brings its block along, and a solid edge marks each side with more to see |
| wheel or trackpad | scroll the page, bringing the caret along when it would leave the page, unless text is selected or typewriter mode is on |
| Ctrl-Shift-Left / Ctrl-Shift-Right | extend the selection a whole word at a time |
| Ctrl-A | select the caret's whole block (again for the whole document) |
| Ctrl-Shift-Up / Ctrl-Shift-Down | move the caret's list item, or the selected items, a place up / down in its list, with the items nested under it |
| Ctrl-F | find — open the find bar |
| Ctrl-= or Ctrl-+ / Ctrl - / Ctrl-0 | increase / decrease / reset the type size |
| Ctrl-Alt + / Ctrl-Alt - / Ctrl-Alt-0 | increase / decrease / reset the interface's text: the menus, title and status row, the notebook's browser and lists, the palette, the help and the dialogs, but not the page. Ctrl-Alt-= works as Ctrl-Alt-+ does. On a Mac these are Cmd-Option-+, Cmd-Option-- and Cmd-Option-0 |
| drag, shift-click, Shift-arrows | select / extend the selection |
| Escape | close the find bar, a dialog, or the note popover |
| F1 | toggle the help dialog: every command in columns as wide as the window allows, scrolling when the window is short |
| Ctrl-P | the command palette: every menu command that can be run here, with its key, narrowed by typing |

## Selecting

Double-click a word to select it, and keep the button down on the second click to drag out by
whole words. Drag or Shift-click to select a range by character.

## Moving and deleting by word

| Key | Action |
| --- | --- |
| Ctrl-Left/Right or Alt-Left/Right | move by word; add Shift to select |
| Ctrl-Backspace, Alt-Backspace or Esc then Backspace | delete the word before the caret |
| Esc then `<` / Esc then `>` | go to the very start / the very end of the document |

Moving right stops after a block's last word and any punctuation behind it, and the next move goes on to the next block.

Esc counts as a prefix when it has nothing to close, and for two seconds.

## Nested lists

In a list item, Tab moves it a level in and Shift-Tab a level out, and with several items
selected they all move together. An item can only go one level below the item before it,
so a list's first item stays put. An item's children stay where they are when it moves.
Ctrl-Shift-Up and Ctrl-Shift-Down move an item a place up or down in its list, past the item before
or after it at its level, and each takes the items nested under it. With several items selected they
move together, at the level of the first. At either end of its list an item stays put: a move never
leaves the list or changes an item's level. Bullets and numbers at the same level are two lists, as they
export, so an item never moves from a bulleted list into a numbered one right after it, or back. The caret stays on the item's words, numbers follow the
new order, and notes, footnotes and marginals go with the item. A move is one undo step.
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
caret. Straight after punctuation that follows a word, they take that word and the punctuation, so a
footnote's number stays after the punctuation and what's typed next goes after it. Ctrl-Shift-M adds a marginal, which belongs to a whole block, and turns Hide margin off if it was on. All
three are on the Annotate menu and the right-click menu. A note needs no words, in which case it's just a highlight.

Each opens for writing as it's added, with any starting words selected so that typing replaces them. Ctrl-Enter, Close or Escape closes it, and Remove takes it
out, along with its highlight. Emptying a marginal removes it too, and so does closing one that nothing was written in, which leaves nothing to undo or redo.

Right-clicking a marginal offers Move to the caret's block, which moves it, words and all, to the block the caret is in,
as one undo step. It's greyed out when the marginal is already there. Any block can hold a marginal, and one moved beside
another stacks below it.

Copy, cut and paste carry them, within a document or into another. A note or footnote goes when the copy holds all of the
words it's on, and one only partly copied stays where it is. A marginal goes when the copy holds all of its block's words,
and lands on the block those words are pasted into, beside any marginal that's there already. Cut and paste moves them,
cutting all of a block's words takes its marginal even though the empty block stays, and copy and paste gives the pasted
words copies of their own. Text copied from outside kame brings none.

## Centered headings

Right-click a heading for Centered, which sets it in the middle of the line, on the page and in HTML, as one undoable edit, and a second click turns it off. Changing the heading's level keeps it, and turning it into body text drops it. If the document's title is centered, its byline is too, and on a site so are the title the page prints and the line under it. Markdown has no form for this, so it exports and imports headings at the left.

## Images

Insert image… on the right-click menu asks for a file, starting in the notebook's `images/` if it has one, or the document's folder, and shows it at its own size, or at the body's width if it's wider. An image from outside the notebook is copied to `images/path/to/document/`, and a different picture with the same name becomes `name-2.jpg`. Nothing in `images/` is public until a published document shows it, and then it publishes at `/static/images/`.

Right-click an image for Align, which puts it at the left, center or right of its line, on the page and in HTML, as one undoable edit. Text never runs beside it, and an image as wide as the body looks the same in all three. Markdown has no form for this, so it exports and imports images at the left.

The document keeps the way to the image from its own folder, so it still shows on another machine and after the notebook moves. A document with no file yet, or outside a notebook, keeps the image's whole path. Saving a document somewhere else says its images' paths again from there, and an export uses whole paths, since it can land anywhere. A document whose images already have whole paths keeps them until it's saved somewhere else.

## Distraction-free modes

Every mode is view-only UI state: never persisted, never an undo step, and
typing is unaffected while any of them is on. The status row shows a badge for
each active mode: `FS` fullscreen, `M` margin hidden, `F` paragraph focus and `T`
typewriter. Hovering one names it.

| Key | Mode | Effect |
| --- | --- | --- |
| F11 | fullscreen | toggles the window fullscreen |
| Ctrl-Shift-H | hide chrome | hides the menu bar and the status row |
| Ctrl-Shift-G | hide margin | collapses the margin gutter; marginal notes and note markers are not drawn (their anchored ranges stay marked, footnotes stay at the foot) |
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
names the document and becomes its opening heading. A link to a heading, such as `[x](<#The Intro>)` or `[x](#The%20Intro)`, becomes a link to that heading in the document. A table becomes a `markdown` code block holding its Markdown exactly as written, and raw HTML becomes
plain text; image descriptions are dropped; the status line reports what was simplified and any images it could
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
there, leaving the old file as it was. Images and relative links are said again from
the new folder, so they lead where they did, all through the history: it isn't an undo
step, and what Redo could bring back is still there. Snapshots say them again too, keeping
their labels and times, so restoring one leads where it did.
Saving over an existing file replaces it whole.

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

A document holding something a newer version of kame wrote opens read-only, and the status row says
to update kame. What this version can't show is left out: a block of a kind it doesn't know reads
"A newer version of kame wrote this part. Update kame to see it.", and a style or note it doesn't
know isn't shown. Nothing is written to the document, not even a snapshot or where you were, and
Save As is refused. An export or a copy leaves those placeholder paragraphs out and says that parts
were left out. A snapshot taken by a newer version can't be restored here.

Clicking a footnote's number drops down to the footnote, and clicking the number at the start of the footnote goes
back to its place in the text. Clicking the footnote's words opens it for editing.

## Coming back

Launching kame without naming a file reopens the last file you had open, if it is
still there. After Ctrl-N, or after importing Markdown, it starts empty instead, until
that document is saved or another file is opened. A second kame started while one is running begins empty instead, as the
first most likely has that document.

A document is only ever open in one kame. Opening one that another kame window has, by any route, from the command line or
the desktop to a link, leaves the document on the screen as it was and says "NAME is open in another kame window", and a
launch onto one starts empty with the same words. A Save As onto it, or a merge with it, is refused the same way.

Each document keeps its caret and scroll position in its own file, and returns to them
whenever it is opened.

## Links between documents

File > Link to Document…, or the right-click menu, asks for a file and links to it, as
`~/path/to/essay.kame` when it is under your home folder. In a notebook, a document inside it is linked by the way from this document's folder, such as `../notes/essay.kame`, which still holds when the notebook moves. With text selected, the selected
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

Ctrl-J, or View > Jump to heading, lists the open document's headings, indented by level, in any document. The letters you type only
have to appear in order, and the arrows, Enter and Escape work as they do in the notebook's lists. Ctrl-[ comes back.

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
kame. A link to a heading in the same document stays a link in Markdown, as
`[words](<#The Intro>)`, so that importing it again gives the same link. In HTML, where
headings have no ids, it is its text.

HTML shows the author under the first heading, and passages highlighted with no note on them. A
note with words leaves no trace, neither the words nor the highlight. Markdown has no highlight, so those passages are plain text there. HTML
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
export, snapshots, document properties, and the document's counts), Edit, Annotate, View (type size,
interface text, dark mode, hyphenation, spelling, a notebook's commands and the view modes) and Help.

The Edit menu has Undo and Redo; Cut, Copy, Paste and Select All; Find; Heading 1, 2 and 3 and
Body Text; Link…; and Move Item Up, Move Item Down, Indent Item and Outdent Item. Each does what its
key does, so Cut, Copy and Paste carry kame's own formatting, notes and links as Ctrl-X, Ctrl-C and
Ctrl-V do, and Select All takes the caret's block, then the whole document. What can't act is greyed
out: Undo or Redo with nothing to do, Cut and Copy with nothing selected, Link… with no words chosen
in one block and no link under the caret, and the list items' commands outside a list. Only the
system knows whether there's anything to paste, so Paste can always be chosen, unless the document
can't be changed. That's one that's read-only because a newer kame wrote it, one whose last save
failed, until it's retried, and one whose file changed on disk or another kame has open, until it's
saved under another name. There everything that would change it is greyed out, and hovering says
which of those it is. Its keys are refused too, Ctrl-X and Ctrl-K included, so a cut never only
copies. Copy, Select All and Find still work. While a dialog is open, the rows that would change the
document are greyed out as well, as their keys go to the dialog. A menu longer than the window has
room for scrolls.

The View menu's first two rows are the type size and the interface's text, each with buttons to
step it that leave the menu open, so either can be walked to taste. The interface text goes from 80%
to 200% in steps of 10%, and Reset puts it back at 100%, as egui draws it. Both are kame settings,
kept for the next launch, and neither is kept in a document. The page and its margin, and what
links here below them, follow the type size, and everything else follows the interface text.

On Linux, Alt-F, Alt-E, Alt-A, Alt-V, Alt-S (in a notebook) and Alt-H open those menus; arrows and Enter then
choose an item, and Escape closes it. There are no such chords on macOS, where
Option plus a letter types a character. The document takes no keystrokes while a
menu is open.

Ctrl-P, or Help > Commands…, opens the command palette, which lists the menus' commands as
"File: Save" with each one's key. It's the same list the menus are drawn from, leaving out what's greyed
out there, such as Cite… outside a notebook. The letters you type only have to appear in order, in the
command or its menu, and the arrows, Enter and Escape work as they do in the other lists. Enter or a
click runs the command as its menu would. Like those lists, it waits while another dialog is up, a
note's box has the keyboard, or the close prompt is asking, and it takes the place of another list.

The status row below it shows the document's title (hover for its path), `not
saved yet` if it has no file, any mode badges, and the word count.

A notice shows in the row, cut to fit with the full text on hover, until the next edit or undo. An error stays until
it is resolved.

## Other versions of a document

If a sync tool leaves a conflict copy beside the open document, the status row says so and File >
Merge Conflict Copies merges it. Sentences both versions changed keep the words on screen, with the
other version in a note. A merge is one undo, after a snapshot, and the copy stays beside the document
with `.merged` added to its name. A copy another kame window has open isn't merged, and kame says "could not merge — NAME is open in
another kame window". Nor is a copy written by a newer version of kame, which stays as it is, and kame says to update kame to merge it, nor a copy into a
document from a newer version.

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
| Ctrl-Shift-O | the browser, a folder tree on the left, with a References row below it: click a key to edit it, right-click one to rename or delete it, or add one without citing it. If the text and its margin don't fit beside it, the page hides until you choose something or close it |

Both lists take the arrows, Enter and Escape. The browser and the jump list notice what arrives from another machine within about 10 seconds of Syncthing delivering it. Ctrl-[ comes back from anywhere they lead.

Following a link to a document that isn't there creates it, inside the notebook only, as a `.kame` with any folders it needs. A heading in
the link becomes its first heading, and a link to a folder makes that folder's `Notebook.kame`. Leave it unwritten and it's removed again.
A document comes before a folder of the same name: `b` leads to `b.kame` if there is one, beside a folder `b/` or not, and `b/` to the folder.
A link to any other kind of file opens it in the system's viewer, and a missing one says so.

Below the footnotes, "Linked from" lists the documents linking to this one. It's only drawn, and never reaches an export or a site.

What's searched is what this machine has. With revision logs, a closed document can be behind what another machine wrote until it's next
opened.

Right-click a document in the browser for Rename…, Move… and Delete…. Folders and other files have no menu.

Rename and Move open one dialog holding the document's path from the notebook's root. Rename selects its name and Move selects its folders,
so you can type over either. A document at the root has no folders to select, so what you type goes in front of its name, and Move says to
type a folder and `/` before the name. Enter confirms, and Escape cancels. Folders it names are created. If something is already at the new
path, or it isn't a place a document can be, the dialog says why and stays open. Nothing is ever overwritten: delete the other file first.

A rename or move rewrites the links to the document in every other document, including closed ones and any with links only in their
revision logs, and fixes its own relative links and images if it changed folders. A title that was only the file's name follows the new
name. `.md` and `.markdown` documents are imported rather than edited, so links in them are left as they are. Those, and any other document
that couldn't be changed, are named in a report when it's done. A document written by a newer version of kame is never changed, and is
named, with "it was written by a newer version of kame; update kame to change it", or, if nothing this kame can read of it links there,
"…so it may link here in parts this kame can't read; update kame to change it". So is one whose logs hold a newer version's revision. If another kame window has the document open, a conflict or retired copy
of it, or any document whose links it would change, the dialog says "NAME is open in another kame window", and no file moves and no link is
rewritten. A copy of it open in this window is refused the same way, as "NAME is open in this window".

Delete asks once, and it's permanent. It takes the document, its logs, and any conflict or retired copies of it. A retired copy is told by
the document's id, as a conflict copy is, so a `.X.retired.kame` holding some other document, left when the path was reused, stays, whether
you delete `X` or move it. Links to it stay, and following one offers to create it again. If it was open, you land on the contents page, or a
new document if it was the contents page. A document another kame window has open, or a copy of it, isn't deleted, and kame says "NAME is
open in another kame window", or "NAME is open in this window" for a copy open here. While a reference is being renamed, Rename…, Move… and
Delete… wait for it to finish.

Deleting a stale copy, one left behind when its document was moved or deleted on another machine, leaves any logs at its path, since those may
belong to whatever document is there now, and the notice says so. The row stays in the browser for those logs. Deleting it again removes
them, on every machine.

In a folder with revision logs, each of these reaches the other machines through the logs. A machine's own database doesn't travel, so on
another machine the stale one is dealt with once kame has seen it stranded for 10 minutes: it's renamed to join its document if that moved.
If the document was deleted, or has already been opened there, the old database is retired. It's only ever renamed, to `.X.retired.kame`
beside where it was (`.X.retired-2.kame` if that's taken), and a notice says where and whether it holds words that never synced. Those files
start with a dot, so the browser doesn't show them. `ls -a` finds them, and File > Open Path opens one. A database open in this window or
another kame window is left until it has been closed. One written by a newer version of kame is left as it is, for an updated kame.

If a log that could be its home can't all be read, a document with nowhere to go isn't retired, unless it would be retired all the same, and a notice says which logs are in the way.

A copy made by hand or in Finder is a document of its own. It starts logs of its own when it's opened, and nothing retires it, since a database
only counts as left behind at the place it last synced. The exception is a copy put at the path of a document with logs, or at the same path in
another notebook with logs turned on and a folder of the same name. Over another document's logs it says they may be another document's and doesn't sync, and Save As gives it
logs of its own. A delete leaves those logs, and a move is refused. In another notebook it can't be told from a stale copy, and is retired, which is undone as below.
A document that last synced before kame recorded where documents are, with no logs beside it at all, may have been moved or deleted on another machine, so it doesn't sync, and says so. Save As gives it logs of its own.

To get a retired file back, rename it to a `.kame` name of your choice, without the leading dot, and open it. It's then a document of its own, with
its own logs. The name it left is the one that doesn't work if its logs are still gone, as that's where it was left behind, and it's retired
again. Opened under its retired name, it stays out of the logs and says it isn't syncing.

### The site tool

`kame site ...` is `kame-site ...`, with no window, so the app is all you need to have. Run under the name `kame-site` it behaves as that
too, so on a Mac a link keeps the two in step:

```sh
ln -sf ~/Applications/kame.app/Contents/MacOS/kame ~/.local/bin/kame-site
```

A file named `site` opens as `kame ./site`.

`kame --version` (or `-V`) prints `kame VERSION` and exits, opening no window and reading no settings; `kame-site --version` and
`kame site --version` print `kame-site VERSION`. The Help window (F1) starts with `kame VERSION` too. A document named
`--version` opens as `kame ./--version`.

### Publishing a notebook

`kame-site build FOLDER` builds it as its own site, in an `output/` folder in the notebook, which the browser and search leave out. In a
synced folder, stop that syncing with `/path/to/notebook/output` and `/path/to/notebook/.output-*` in `.stignore`, or two machines' builds
will conflict. A build won't replace an `output/` it didn't make.

In a notebook, the Site menu has Build and Sync, which run in the background and list any warnings when they finish. A document written by
a newer version of kame publishes what this kame can read of it, and the build warns that it may be missing parts. Sync needs your key in an agent, since kame can't ask for a passphrase.

`kame-site sync FOLDER` also sends it to `remote` with `rsync --delete`. A dry run comes first, and if that would delete more than 5 files
from the remote, they're listed and nothing happens without `--yes`. Document Properties has a *Publish* tick, and in Markdown it's `publish: true` in the front matter. `Notebook.kame`
in the root is the home page and always publishes.

URLs follow the folders, so `maritain/materialism.kame` is `/maritain/materialism/`, and a marked `Notebook.kame` in a folder is that
folder's page. Everything in `static/` publishes at `/static/`, whether or not anything links to it, and isn't searched. Anything not for
publication goes in another folder. A link to an unmarked document, or to a file outside `static/`, publishes as its words, and the build
lists it. A link to something missing under `static/` or to a folder publishes as written, for the web server to answer, and any other is its words. An image from `images/` shown in a published
document publishes at `/static/images/`, any other in the notebook is copied beside its page, and one outside the notebook doesn't publish, which the build also lists.

The theme is the standard one, and a file in the notebook's `theme/` takes the place of the same file in it. Its About link leads to an `about` document or page in the root, however its name is capitalised, and doesn't show without one. With no `url` there's no feed.

## Revision logs

This is a trial, for a folder at a time. `kame-sync enable FOLDER` puts a `.kame-logs` file in a folder, and prints the lines that every
machine's `.stignore` needs. Documents under that folder then stop syncing their databases, and each machine keeps its own log of
revisions in `.X.kame.sync/`, which is what syncs.

kame writes a revision on a pause in typing, on losing focus and on leaving a document. What arrives from another machine shows up on
a pause or on coming back to the window, never mid-word, and the status row says where it came from. A copy left open follows along,
with everything that arrives between your own edits as one undo step. Sentences both machines changed keep the later
version in the text and the other in a note, after a snapshot. A document with only logs, as on a new machine, is built when opened,
unless only a newer version of kame can read them, when it isn't opened and the status row says to update kame, or unless they hold only
changes that build on others that aren't here, when it isn't opened. They may still arrive. If it was moved or deleted on
another machine they won't: opening it on the machine these changes came from, which its log is named for, brings the rest
here. Deleting it in the browser removes what's left, and outside a notebook, so does removing its `.X.kame.sync/` folder by
hand. Nor is one whose logs hold only lines kame can't use, damaged or in a form it can't check, and if they're damaged, the same
removes them.
Two documents of one name merge too, as when two machines each make it from the same link before either's logs reach the other, or
a document deleted and made again on one machine meets the old one's log. Both keep all their words, and the status row says
"merged with another document of this name from" the machine, once. Undo takes the other's words out again.
If a document's logs can't be read, it doesn't sync, and a notice says why, once. If another machine writes with a newer version of kame, this one says so and the document goes read-only until kame is updated here.

Snapshots and undo history stay on the machine that made them. To go back, remove the folder's `.stignore` line, then delete `.kame-logs` and the
hidden `.sync` folders.
