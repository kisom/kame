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

- `# `, `## ` or `### ` at the start of a line makes a heading. Ctrl-Shift-1, -2 and -3 do the same, and Ctrl-Shift-0 returns to body text.
- `- ` or `1. ` at the start of a line starts a list. Tab moves an item in a level and Shift-Tab moves it out. Enter on an empty item comes out of the list.
- `*word*` is italic, `**word**` is bold, and a pair of backticks around a word makes it inline code.
- `> ` at the start of a line is a block quote.
- `---` then Enter is a horizontal rule.
- Three backticks then Enter starts a code block, and you can put a language straight after them. Press Enter on a blank last line to leave it.
- `| ` at the start of a line starts a prose block, for poetry or anything else whose line breaks and spacing should stand exactly as typed. It leaves the same way as a code block.
- `[words](https://example.com)` is a link. So is Ctrl-K on a selection, where an empty address removes the link.

The right-click menu changes the kind of the block under the caret, adds annotations, and has Insert image…, which asks for a file and shows the picture at its own size, or at the body's width if it's wider. "Where to put papers and images" below says where the file ends up.

To link to another document, type `[[essay]]`, `[[notes/essay#The Intro]]` or `[[notes/essay#The Intro|the intro]]`, where the words after `|` are what shows. Paths start from the folder of the document holding the link. File > Link to Document… picks the file for you, but under your home folder it writes the path as `~/...`, which breaks if the notebook moves or reaches a machine with a different home folder. In a notebook, typing the link is safer. Clicking a document link opens it, and Ctrl-[ takes you back, as many links as you went.

## The margin: notes, marginals and footnotes

These are the reason for kame's name. Each attaches to the selection, or to the word under the caret, and stays with those words as the text changes around them. They differ mostly in who gets to read them.

- A **note** (Ctrl-Shift-N) highlights a passage and holds a comment for you alone. The note's words never publish, in any format. The highlight does show in HTML, so a reader can see that you marked the passage but not what you said about it.
- A **marginal** (Annotate > Marginal, or the right-click menu) sits in the margin beside its paragraph. It publishes in HTML, and is left out of Markdown, which has nowhere to put it.
- A **footnote** (Ctrl-Shift-F) is numbered and collected at the foot of the page, and publishes in both formats. Its selection grows to whole words.

Adding one opens a small box for its words, and Escape closes that box. Click a note's marker to open it again, or a footnote's words to edit them. The box has a Remove button. Clicking a footnote's number moves between the footnote and its place in the text.

## Getting around and keeping focus

Ctrl-F finds text in the document, with Enter for the next match and Shift-Enter for the one before. Ctrl-Left and Ctrl-Right move by word, and adding Shift selects as you go. Ctrl-A selects the block, and again the whole document. Esc then `<` or `>` goes to the very start or end.

When you want less on the screen, each of these is a toggle, and none of them changes the document:

- F11: fullscreen.
- Ctrl-Shift-H: hide the menu bar and status row.
- Ctrl-Shift-M: hide the margin.
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

- HTML keeps kame's type and colours, follows the reader's light or dark setting, and carries the footnotes, marginals and note highlights.
- Markdown carries the footnotes, opens with a `Title:`, `Date:`, `Author:` and `Tags:` header for blog tools such as Pelican, and turns highlights back into plain text.
- A link to a kame document exports as its words, since it can't lead anywhere outside kame.
- An image exports with its whole path, so the page only shows it while the picture stays where it is on this machine.

Opening a `.md` file imports it as a new document and leaves the file alone. The status row says what it had to simplify. Tables and raw HTML become plain text, and image descriptions are dropped. Front matter fills in the properties.

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
- **Ctrl-Shift-O** opens the browser, a folder tree on the left. If the page doesn't fit beside it, the page hides until you choose something or close it.

Links are how a notebook grows. Following a link to a document that isn't there creates it, with any folders it needs, and if you leave it unwritten it's removed again. So you can type `[[maritain/materialism]]`, click it and start writing. A link to a folder opens or creates that folder's own `Notebook.kame`. Links can only create documents inside the notebook.

Below the footnotes, "Linked from" lists the documents that link to this one. It's only drawn on screen, and never reaches an export or a site.

kame has no Move command yet. If you move or rename a document, links to it don't follow, and its published address changes with it.

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

**Make an About page.** The standard theme's navigation has an About link, which leads nowhere until you make `about.kame` in the notebook's root and mark it to publish. The site also gets an archive and a tags page, and a feed if you've set `url`.

**To send it to your server**, set `remote` in `notebook.yaml`, then:

```sh
kame-site sync ~/writing/garden
```

This builds the site and sends it with `rsync --delete`, so `rsync` must be installed, and anything else in that folder on the server is removed. A dry run comes first, and if it would delete more than 5 files, it lists them and stops until you run it again with `--yes`.

**To change the look**, copy a file from the standard theme into the notebook's `theme/` at the same path, and yours takes its place. The theme's files are in kame's source, under `crates/kame-site/theme/`, so the stylesheet goes at `theme/static/css/site.css` and the shared page layout at `theme/templates/base.html`.

**Before publishing, it's worth remembering** that highlights from private notes show, though their words don't, and that everything in `static/` goes out. Everything else stays home, unless a published document shows it as an image.

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
- Ctrl-K: link
- Ctrl-F: find in this document
- Ctrl-L: jump to a document in the notebook
- Ctrl-R: search the notebook
- Ctrl-Shift-L: the notebook's contents page
- Ctrl-Shift-O: the notebook's browser
- Ctrl-[: back
- Ctrl-Shift-S: snapshot
- Ctrl-Shift-E and Ctrl-Shift-B: export Markdown and HTML
- F1: help

The full list, with every detail, is in `docs/keybindings.md` in kame's source.
