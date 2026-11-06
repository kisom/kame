---
title: The kame user's guide
---

kame is an editor for writing and thinking on the page. You type straight into the typeset page, and write beside it in the margin. There's no source view: Markdown and HTML only come into it when you export. A document is a `.kame` file holding its text, its whole edit history and any snapshots you've taken.

Everything here is on Linux or macOS. Wherever this guide says Ctrl, a Mac uses Cmd.

This guide covers:

- Getting it
- Your first document
- Writing
- The margin: notes, marginals and footnotes
- Getting around and keeping focus
- History and snapshots
- Moving words in and out
- Notebooks
- References
- Where to put papers and images
- Building a site
- Using more than one machine
- Settings
- The keys worth learning first

## Getting it

You build kame from source, which takes a Rust toolchain and a C compiler. On Linux you also need `pkg-config` and the X11, Wayland and Vulkan development libraries. The project's `flake.nix` lists them, and on NixOS `nix develop` provides them.

```sh
git clone https://github.com/kisom/kame
cd kame
```

On Linux, install it with:

```sh
make install
```

That puts `kame` and `kame-site` in `~/.local/bin`, adds a desktop launcher and registers `.kame` files. Make sure `~/.local/bin` is on your `PATH`.

On a Mac, build the app and put it in place:

```sh
make mac-app
mkdir -p ~/Applications
cp -R target/kame.app ~/Applications/
```

Run it as an app, since the bare program has no menu bar and won't take keyboard focus. The site tool is inside the app, and a link gives it its usual name at the command line:

```sh
mkdir -p ~/.local/bin
ln -sf ~/Applications/kame.app/Contents/MacOS/kame ~/.local/bin/kame-site
```

The app is signed only for your own machine, so build it on the Mac you'll use it on.

## Your first document

Start kame and type. The first Ctrl-S asks where to put the file, and from then on every edit reaches the disk as you make it, so there's nothing more to save. You can also start from the command line:

```sh
kame                # the last document you had open
kame essay.kame     # open this one, or create it
kame notes.md       # import Markdown as a new, unsaved document
```

Each window holds one document, and opening another replaces it. A second kame started while one is running begins empty, so that two windows don't overwrite each other's edits on one file. A document remembers where your caret and scroll position were.

The menus are File, Annotate, View and Help, and F1 shows a short list of keys. On Linux, Alt-F, Alt-A, Alt-V and Alt-H open the menus from the keyboard.

File > Document Properties holds the title, author, dates and tags. A document with no title goes by its file's name.

## Writing

Formatting applies as you type, and undoes as one step.

- `# `, `## ` or `### ` at the start of a line makes a heading. Ctrl-Shift-1, -2 and -3 do the same, and Ctrl-Shift-0 returns to body text. Right-click a heading and choose Centered to set it in the middle of the line, which Markdown exports leave out.
- `- ` or `1. ` at the start of a line starts a list. Tab moves an item in a level and Shift-Tab moves it out. Enter on an empty item comes out of the list.
- `*word*` is italic, `**word**` is bold, and a pair of backticks around a word makes it inline code.
- `> ` at the start of a line is a block quote.
- `---` then Enter is a horizontal rule.
- Three backticks then Enter starts a code block, and you can put a language straight after them. Press Enter on a blank last line to leave it.
- `| ` at the start of a line starts a prose block, for poetry or anything else whose line breaks and spacing should stand exactly as typed. It leaves the same way as a code block.
- `[words](https://example.com)` is a link. So is Ctrl-K on a selection, which also lets you change the words: left empty, they show the address, and an empty address removes the link. Right-click a link to open it, copy it or edit it. Cutting or copying within kame keeps links, formatting and annotations when you paste, even in another document, and a relative link still leads to the same place from there.

The right-click menu changes the kind of the block under the caret, adds annotations, and has Insert image…, which asks for a file and shows the picture at its own size, or at the body's width if it's wider. Right-click a picture for Align, to put it at the left, center or right of its line. It's on a line of its own, whichever you choose, and Markdown exports leave the alignment out. "Where to put papers and images" below says where the file ends up.

**Ctrl-J** lists the headings of the open document, in or out of a notebook, indented by level. Type a few letters to narrow them, and press Enter to go there. Ctrl-[ comes back.

To link to another document, type `[[essay]]`, `[[notes/essay#The Intro]]` or `[[notes/essay#The Intro|the intro]]`, where the words after `|` are what shows. Paths start from the folder of the document holding the link. File > Link to Document… picks the file for you, but under your home folder it writes the path as `~/...`, which breaks if the notebook moves or reaches a machine with a different home folder. In a notebook, typing the link is safer. Clicking a document link opens it, and Ctrl-[ takes you back, as many links as you went.

## The margin: notes, marginals and footnotes

These are the reason for kame's name. Each attaches to the selection, or to the word under the caret, and stays with those words as the text changes around them. Straight after punctuation, the word is the one before it, so a footnote added at the end of a sentence keeps its number after the full stop as you type on. They differ mostly in who gets to read them.

- A **note** (Ctrl-Shift-N) highlights a passage and holds a comment for you alone. The note's words never publish, in any format, and neither does its highlight. Leave a note without words and it's purely a highlight, which does show in HTML.
- A **marginal** (Ctrl-Shift-M) sits in the margin beside its paragraph. It publishes in HTML, and is left out of Markdown, which has nowhere to put it.
- A **footnote** (Ctrl-Shift-F) is numbered and collected at the foot of the page, and publishes in both formats. Its selection grows to whole words.

Adding one opens a small box for its words, and Escape closes that box. Click a note's marker to open it again, or a footnote's words to edit them. The box has a Remove button. Clicking a footnote's number moves between the footnote and its place in the text. To move a marginal to another paragraph, put the caret there, right-click the marginal and choose Move to the caret's block. One undo puts it back. Cutting and pasting a passage takes its notes and footnotes along, and its paragraph's marginal if you cut the whole paragraph. Copying gives the pasted words copies of their own. A note on words only partly copied stays where it was.

## Getting around and keeping focus

Ctrl-P lists every menu command with its key. Type a few letters of one, such as "exp ht" for Export HTML, and press Enter to run it.

Ctrl-F finds text in the document, with Enter for the next match and Shift-Enter for the one before. Ctrl-Left and Ctrl-Right move by word, and adding Shift selects as you go. Ctrl-A selects the block, and again the whole document. Esc then `<` or `>` goes to the very start or end.

When you want less on the screen, each of these is a toggle, and none of them changes the document:

- F11: fullscreen.
- Ctrl-Shift-H: hide the menu bar and status row.
- Ctrl-Shift-G: hide the margin. Adding a marginal shows it again.
- Ctrl-Shift-P: dim everything but the current paragraph.
- Ctrl-Shift-T: typewriter mode, which keeps your line at the middle of the window.

Ctrl-Shift-D is dark mode, and Ctrl-plus, Ctrl-minus and Ctrl-0 change the type size. View > Check spelling marks likely mistakes in red, and other things worth a look in blue. Right-click a mark for suggestions, or to accept it, add the word or turn the rule off. F7 goes to the next mark.

## History and snapshots

Undo (Ctrl-Z) and redo (Ctrl-Shift-Z) work a word at a time, and the history lives in the file, so it survives closing the document.

A snapshot is a complete copy of the document, stored in its file. Take one with Ctrl-Shift-S before a risky revision, with a label if you like. Ctrl-Shift-R lists them. Restoring one asks first, because it discards the current text and starts a fresh history. Snapshots themselves are kept, so you can undo a restore by restoring a newer one.

**A `.kame` file carries all of this with it.** Sending someone the file sends them your history, your snapshots and your private notes, including everything you've since deleted. To share what a document says, export it or publish it instead.

File > Compact History shrinks files saved by older versions of kame. It loses only the stopping points inside words.

## Moving words in and out

Ctrl-Shift-E exports Markdown and Ctrl-Shift-B exports a standalone HTML page. Each asks where to write, and neither needs the document saved first. Exports are the same every time for the same document, so they sit well in version control.

- HTML keeps kame's type and colours, follows the reader's light or dark setting, and carries the footnotes, marginals and highlights without notes.
- Markdown carries the footnotes, opens with a `Title:`, `Date:`, `Author:` and `Tags:` header for blog tools such as Pelican, and turns highlights back into plain text.
- A link to a kame document exports as its words, since it can't lead anywhere outside kame.
- An image exports with its whole path, so the page only shows it while the picture stays where it is on this machine.

Opening a `.md` file imports it as a new document and leaves the file alone. The status row says what it had to simplify. Tables and raw HTML become plain text, and image descriptions are dropped. Front matter fills in the properties. A relative link leads from the Markdown file's folder, and still leads there once the document is saved, wherever that is.

## Notebooks

A notebook is a folder of documents that kame treats as one body of work, with a jump list, search and a browser, and it can be published as a site. Everything in it stays an ordinary file: there's no index or database to go stale, and you can arrange the folder however you like.

Make one with File > New Notebook…, which turns a folder you choose into a notebook, keeps anything already in it, and opens its new contents page. Or do it from the command line:

```sh
kame-site new-notebook ~/writing/garden
kame ~/writing/garden
```

What makes a folder a notebook is a file named `notebook.yaml` in it, and opening any document under that folder opens the notebook. The file can be empty. It has four optional settings, which only matter for a title or for publishing:

```yaml
title: The Garden
url: https://example.net/
author: Your Name
remote: you@example.net:/srv/garden/
```

Getting around takes four keys, also in the View menu:

- **Ctrl-L** jumps to a document by title or path. The letters you type only have to appear in order. If nothing matches, the last row offers to create it.
- **Ctrl-Shift-L** goes to `Notebook.kame`, the contents page. kame doesn't write this page for you: it's a document like any other, and you fill it with whatever links and words suit you.
- **Ctrl-R** searches every document, including margin notes, footnotes and private notes, and lists every match.
- **Ctrl-Shift-O** opens the browser, a folder tree on the left, with the notebook's references below it. If the page doesn't fit beside it, the page hides until you choose something or close it.

Links are how a notebook grows. Following a link to a document that isn't there creates it, with any folders it needs, and if you leave it unwritten it's removed again. So you can type `[[maritain/materialism]]`, click it and start writing. A link to a folder opens or creates that folder's own `Notebook.kame`. A document of the same name comes first, so `[[b]]` stays on `b.kame` once there's a folder `b/` beside it, and `[[b/]]` is the folder. Links can only create documents inside the notebook.

Below the footnotes, "Linked from" lists the documents that link to this one. It's only drawn on screen, and never reaches an export or a site.

To rename, move or delete a document in a notebook, right-click it in the browser. Links to it follow a rename or a move, and its published address changes with it. Move or rename the file by hand and links to it don't follow. A document open in another kame window can't be moved or deleted, and a document can't be moved while one linking to it is open in another kame window; close it there first.

## References

A notebook keeps its sources in one list, and you cite one by typing its key in square brackets, such as `[knuth1981]`. Exports then end with a References section holding what that document cited. Nothing here changes how the page looks while you write, apart from a citation's colour.

**Citing.** A key is ASCII letters, digits, `-` and `_`, starting with a letter, up to 64 bytes. So `[1]` is never a citation. Keys are case-sensitive, and `[[target]]` and `[text](url)` are links, not citations. The text stays exactly as you typed it, and a citation is just those brackets around a known key, so a source you add later makes earlier `[key]` text a citation without touching the document. Citations count in paragraphs, headings, list items, block quotes, footnotes and marginals. They don't count in code blocks, prose blocks, inline code, a link's own words or notes, since a note never publishes. A known key is drawn in the link colour, and clicking it shows its entry in IEEE format, with an Edit button. Spell checking skips it.

When you close a `[key]` that has no reference, the status row says "No reference called kleppmann2019. Ctrl-Shift-C adds it." and nothing more, so `[sic]` costs nothing. The note goes once the key has a reference.

**Ctrl-Shift-C** (Annotate > Cite…, and Cite… in the right-click menu) works in a notebook, and what it opens depends on the caret:

- In or just after a `[key]` with no reference: the new reference window, with that key filled in. Saving leaves the text alone, and it becomes a citation.
- In or just after a known `[key]`: that reference, for editing.
- Anywhere else: a search over keys, authors and titles, ignoring case. Choosing one inserts `[key]` at the caret, as an undo step of its own, apart from anything typed after it. If nothing matches, the last row is a new reference.

Outside a notebook it says "Citing needs a notebook."

**The new reference window** has two tabs. Structured takes a kind (Article, Conference, Book or Web) and its fields, with the names separated by commas, and shows the IEEE entry as you fill it in. A comma before Jr., Sr., II, III or IV keeps it with the name before, so "M. L. King, Jr." is one name. If a name holds any other comma, as "King, M. L." does, separate them with semicolons instead, and end a single such name with one. Freeform takes a key and a box of text, which goes into the References section exactly as typed, for anything the form doesn't fit. The key is suggested from the first author's last name and the year, and you can change it. The button is "Add and cite" when you came from citing, "Add" when you came from the browser or the References… window, which leaves the document alone, and "Save" when editing. A key that's taken, badly formed or missing, or an entry with no title, is refused. The key of an existing reference can't be changed there. While that window or Cite's search is up, kame won't leave the document for another, since what's typed would go with it, and it says so. A file the system asks it to open waits until the window closes, and then opens.

**The browser** (Ctrl-Shift-O) has a References row below its folders, which opens as a folder does. Click a key to edit it, right-click one to rename or delete it, and choose New reference… to add one without citing it. These lead to the same windows as Annotate > References…, and with another window up they ask you to close it first, so nothing you've typed is lost.

**Annotate > References…** lists every reference, with how many documents cite it. Each row has Edit, Rename… and Delete…, and New reference… adds one without citing it.

- Delete… says how many documents cite the reference first. Those documents then hold plain `[key]` text.
- Rename… rewrites `[old]` to `[new]` in each citing document, in prose, footnotes and marginals but not notes, as an ordinary edit there with its own undo step. The window then lists what it did. A document that can't be rewritten, or can't be read to see whether it cites the key, is named with the reason, and the old key is kept, so `[old]` still resolves. A Markdown document is one of those, and if it's imported and open it stays as its file is, and so is one open in another kame window. So is one written by a newer version of kame, even if nothing this kame can read of it cites the key. Run the rename again once the problem is fixed to finish it.

Reference files are written straight to disk and aren't undo steps. A file kame can't use, such as one with a field it doesn't know, is left alone and named in the References… window, and the status row says what's wrong with it for as long as it's so, once anything passing has gone.

**Exporting.** HTML, Markdown and a notebook's site end with a `## References` section, after the footnotes. It lists only what that output cites, sorted by key ignoring case, one entry to a paragraph in IEEE format. In HTML and on a site, each citation links to its entry. In Markdown the citations stay as text. A document citing nothing gets no section and exports as it always did. A section you wrote yourself isn't looked for, so you'd get two. Markdown leaves out marginals, so a source cited only in a marginal appears in HTML and not in Markdown.

**What it can't do yet:**

- It needs a notebook. In a document outside one, `[key]` stays text and exports get no section.
- Citations in footnotes and marginals aren't drawn differently on the page, and the status row doesn't comment on them. Type the key by hand. It takes effect in exports.
- A `[key]` with no reference stays in the export as typed. Only keys shaped like the suggested ones, letters of either case then four digits and an optional letter, such as `smith2020` or `Smith2020`, are warned about, in the export's notice ("exported; no reference for smith2020") and in a site build's report.
- A site built from a `site.yaml` rather than a notebook has no references, and keeps `[key]` as text.
- Importing a Markdown export brings its References section back as an ordinary heading and paragraphs, so exporting again from the notebook gives two sections.
- Renaming a key edits documents that aren't open. In a folder with revision logs, each is first brought up to date with what other machines have written, so a citation only they have is renamed too, and the edit goes to its logs at once. The open document is brought up to date first as well, and if what's arrived in its logs can't be taken in, it's named and the old key is kept. The old key's file can still be gone on another machine before those logs arrive there, leaving `[old]` as plain text for a while. The rename can't be run again there, as the old key has no file. Wait for the rewritten documents to arrive, or add the old key again with the same contents (copying `references/new.yaml` to `references/old.yaml` does it) and rename it to the new key. A second kame with a citing document open can overwrite a rename with its older copy.

## Where to put papers and images

The folders decide what's public, so this is the part of a notebook to get right. A notebook might look like this:

```
garden/
  notebook.yaml
  Notebook.kame        the contents page, and the site's home page
  about.kame
  maritain/
    Notebook.kame
    materialism.kame
  static/              public: all of it publishes
    papers/
      maritain-1944.pdf
    photos/
  papers/              private: for your reading
  images/              pictures, private until a published document shows one
  theme/               your changes to the site's look, if any
  output/              the built site
```

**Anything in `static/` is public.** It all publishes under `/static/`, whether or not anything links to it. kame doesn't search this folder. Before you put a paper in `static/`, make sure it's yours to publish.

**Anywhere else is private, apart from what a published document shows.** Keep papers you only want to read in a folder of your own, such as `papers/`. A link to one opens it in your system's viewer, and on the site that link publishes as plain words.

**Link to a paper with a path from the document you're in**, and press Ctrl-K on the words:

- from `Notebook.kame`: `static/papers/maritain-1944.pdf`
- from `maritain/materialism.kame`: `../static/papers/maritain-1944.pdf`

Don't begin the path with a slash. `/static/...` means the top of your disk rather than the top of the notebook, so the link leads nowhere in kame and publishes as plain words.

**Pictures belong in `images/`, and kame will put them there.** Insert image… starts in that folder if it exists. Choose a picture from outside the notebook and kame copies it to a folder following the document's place, so `maritain/materialism.kame` gets `images/maritain/materialism/`. The original stays where it was, and a different picture with the same name becomes `name-2.jpg`. Save the document first: one with no file yet keeps the picture's whole path and copies nothing.

**Nothing in `images/` is public until a published document shows it.** Then that one picture publishes at `/static/images/`, and the rest of the folder stays home. A picture elsewhere in the notebook is copied beside its page instead. One outside the notebook doesn't publish, and the build tells you so. Use `static/` only for a picture you want public whether or not anything shows it.

**An image stays a separate file.** The document holds the way to it from the document's own folder rather than a copy, so the notebook can move, or reach another machine, and the picture still shows. File > Save As works the path out again from the new place. Moving a `.kame` file by hand or renaming the picture's folder does break it: the picture won't appear, and the build reports it as missing, until you insert it again. Links to papers behave the same way, unless you write them with `~`.

## Building a site

Nothing publishes until you say so. To mark a document, tick *Publish* in File > Document Properties, or in a Markdown file add this to the front matter:

```yaml
publish: true
```

The exception is the root `Notebook.kame`, which is the home page and **always publishes**, so don't keep anything private in its text. A `Notebook.kame` in a folder needs marking like any other, and becomes that folder's page.

Addresses follow the folders, so `maritain/materialism.kame` becomes `/maritain/materialism/`.

To give a document a date on the site, set Published in Document Properties, as `YYYY-MM-DD`. Without one it uses its created date or the file's, and the build mentions it. Tags in the properties get pages of their own.

Build and look at it:

```sh
kame-site build ~/writing/garden
python3 -m http.server -d ~/writing/garden/output
```

Then open `http://localhost:8000`. Opening the files directly won't work, because the pages expect to be at the top of a website.

Read what the build prints. It lists each link that became plain words because it leads to something unpublished, and each image it couldn't find or won't publish. These are warnings, so the site still builds.

**Make an About page.** The standard theme's navigation gains an About link once you make `about.kame` or `About.kame` in the notebook's root and mark it to publish. The site also gets an archive and a tags page, and a feed if you've set `url`.

**To send it to your server**, set `remote` in `notebook.yaml`, then use Site > Sync, or:

```sh
kame-site sync ~/writing/garden
```

This builds the site and sends it with `rsync --delete`, so `rsync` must be installed, and anything else in that folder on the server is removed. A dry run comes first, and if it would delete more than 5 files, it lists them and stops until you say to send anyway, or run it again with `--yes`. kame can't ask for a passphrase, so for Site > Sync your key has to be in an agent.

**To change the look**, copy a file from the standard theme into the notebook's `theme/` at the same path, and yours takes its place. The theme's files are in kame's source, under `crates/kame-site/theme/`, so the stylesheet goes at `theme/static/css/site.css` and the shared page layout at `theme/templates/base.html`.

**Before publishing, it's worth remembering** that a highlight with no note on it shows, and that everything in `static/` goes out. Everything else stays home, unless a published document shows it as an image.

**If you'd rather not use a notebook**, `kame-site new mysite` makes a separate site whose `site.yaml` lists each document and its address, wherever those documents live. It suits publishing a few documents from all over. Its `README.md` and `Makefile` explain the rest, with `make serve` to preview and `make sync` to send.

## Using more than one machine

kame doesn't sync anything itself, but it's built to live in a folder that Syncthing or something like it keeps in step.

If two machines change the same document, the sync tool leaves a conflict copy beside it. The status row says so, and File > Merge Conflict Copies merges them. Sentences both versions changed keep the words on screen, with the other version in a note. If the file is replaced while you have it open, kame stops saving to it and asks whether to merge, save yours under a new name, or open the one on disk.

If a notebook is in a synced folder, keep builds out of the sync, or two machines' builds will conflict. In Syncthing, that means adding these lines to `.stignore`, with your own path:

```
/writing/garden/output
/writing/garden/.output-*
```

There's also a trial of per-folder revision logs, for leaving a copy open on another machine without conflicts. It isn't ready for general use. If you're curious, it's described under "Revision logs" in `docs/keybindings.md`.

## Settings

kame's settings are in `config.json`, at `~/.config/kame/` on Linux and `~/Library/Application Support/kame/` on a Mac. Most of it looks after itself, but these are worth knowing:

- `author` is the author for new documents. "Use this author for new documents" in Document Properties sets it for you.
- `words` is your own spelling list.
- `rules_off` holds the checks you've turned off.

## The keys worth learning first

- Ctrl-Shift-N: note
- Ctrl-Shift-F: footnote
- Ctrl-Shift-C: cite a reference, in a notebook
- Ctrl-K: link
- Ctrl-F: find in this document
- Ctrl-L: jump to a document in the notebook
- Ctrl-R: search the notebook
- Ctrl-Shift-L: the notebook's contents page
- Ctrl-Shift-O: the notebook's browser
- Ctrl-[: back
- Ctrl-J: jump to a heading in this document
- Ctrl-Shift-S: snapshot
- Ctrl-Shift-E and Ctrl-Shift-B: export Markdown and HTML
- Ctrl-P: every menu command, by name
- F1: help

The full list, with every detail, is in `docs/keybindings.md` in kame's source.
