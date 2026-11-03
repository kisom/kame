# Kame notebook — design

Date: 2026-11-03
Status: draft for review. Nothing is built. Section 12 lists three things for you to settle, and section 11 the
decisions made on your behalf. The starting notes are in `MultiKame.kame`.

## 1. Purpose

A notebook is a focused collection of writing and research: tens or hundreds of documents on one subject or
project, kept in one folder, that can be moved through quickly while writing and published as its own site. There
will be several. kame stays a page with very little around it, and everything here appears only when asked for.

It isn't a replacement for Obsidian, a place for all notes, or a Markdown editor.

## 2. What's been decided

| Question | Decision |
|---|---|
| What it's for | a focused collection, not all notes |
| How many | several, one open at a time. An über-notebook is possible someday and isn't designed for |
| The app | a superset of kame, not a separate program |
| What marks a notebook | a `.knb` file, which is also the notebook file proper |
| Its format | YAML to begin with, since it's easy to replace later |
| Contents page | `Notebook.kame` in the root |
| Markdown | minimal, page-like, edited in an external editor such as kge. Import and export are left for later |
| On screen | nothing until asked. A jump list, a search and a hideable browser |
| Publishing | each notebook is its own site, and only marked documents publish |
| Following a link to a missing document | creates it |
| What links here | shown at the bottom of the document |

## 3. A notebook on disk

```text
philosophy/
├── Philosophy.knb
├── Notebook.kame        the contents page, and the site's home page
├── about.md
├── maritain/
│   └── materialism.kame
└── theme/               optional, for a site that differs from the standard theme
```

The `.knb` holds only what can't be worked out from the folder:

```yaml
title: Philosophy
url: https://philosophy.example.net/
author: K. Isom
remote: kyle@host:/srv/philosophy/
```

`title` defaults to the file's name if left out. `url`, `author` and `remote` are only for
publishing. `remote` is the same idea as `UPSTREAM` in a `kame-site` Makefile. Unknown keys are refused, as in
`site.yaml`, so a typo shows up instead of quietly doing nothing.

**Finding the notebook.** Opening any document looks up through its parent folders for a `.knb`, and the nearest one
wins. So there's no separate step to enter a notebook, and a notebook inside another is just its own notebook. Opening
a `.knb` opens its `Notebook.kame`, or the browser if there isn't one. Two `.knb` files in one folder is an error that
the status row reports, and neither applies.

**What counts as a document.** `.kame` and `.md` files under the root, leaving out hidden folders, `theme/`,
conflict copies and retired `.merged` files.

## 4. The code

A new crate, `kame-notebook`, with no GUI, on `kame-core`, in keeping with the layering. It finds a notebook, reads
its `.knb`, lists its documents, searches them, and works out what links where. The app and `kame-site` both use it.

**No index on disk, at first.** Tens or hundreds of documents are few enough to read on a background thread when a
notebook opens, keeping their text and links in memory and rereading a file when its modified time changes. That means
no index to rebuild, sync or corrupt. An earlier draft of this idea had an index on disk, and it's easy to add if
reading proves too slow. Reads use `Db::open_read_only`, and never change a document.

## 5. Jumping: Ctrl-L

A small list opens over the page. Typing narrows it by title and path, with letters needing only to appear in order, and
recently opened documents come first. Enter opens the choice and Escape closes the list, as with the search bar. If
nothing matches, the last row offers to create a document at the typed path, so nothing is created by accident.

## 6. Searching: see section 12

The same kind of list, searching the text of every document in the notebook, including margin notes and footnotes.
Case-insensitive, like Ctrl-F. Each row shows the document and the line around the match, and choosing one opens that
document at the match, with Ctrl-[ going back. It searches the text as saved, plus whatever's on screen for the open
document.

## 7. The browser

A folder tree of the notebook's documents on the left, with the current one highlighted. Clicking a document opens it.

- **Hidden by default.** A key toggles it, as does an item in the View menu, and hide-chrome hides it too. It remembers
  whether it was open and which folders were expanded, in the app's config.
- **It takes the left margin.** The document stays exactly where it is, so nothing rewraps, as long as the text and the
  right margin still fit. If they don't, the document hides and only the browser shows. Choosing a document or closing the
  browser brings the page back.
- **Only in a notebook.**

The key can't be Ctrl-B or Ctrl-Shift-B, which are both taken. I'd use `Ctrl-\`.

## 8. Links

**A missing document.** Following a relative link to a document that doesn't exist creates it, as a `.kame` file, along
with any folders it needs, and opens it. Ctrl-[ comes back. Three rules keep this safe:

- **Only inside the notebook.** A link leading outside it, or any link outside a notebook, says the link has rotted, as
  today.
- **Mistyped links leave nothing behind.** If you leave a new document without writing anything, it's removed, with its
  `-wal` and `-shm`. Only a document created this way, this session, and never edited is ever removed.
- **A heading in the link** becomes the new document's first heading, so that the link then leads somewhere.

**What links here.** Below the footnotes, in the apparatus type, a short list of the documents linking to this one. A
click opens one. It isn't part of the document, so it never reaches exports or the site, and it isn't shown when
empty. It comes from the same reading as search, so it can take a moment to appear after opening.

## 9. Publishing

Each notebook is its own site. `kame-site` learns to build one:

```sh
kame-site build Philosophy.knb
kame-site sync Philosophy.knb    # builds, then sends it to `remote`
```

- **What's marked.** A *Publish* tick in Document Properties, stored in `Metadata` like tags. In Markdown it's
  `publish: true` in the front matter. It's off by default, since notebooks hold research and working notes too.
- **URLs follow the folders.** `maritain/materialism.kame` becomes `/maritain/materialism/`. Moving a file moves its
  URL, which differs from `/k/` on your essays site, and the build lists what moved compared with the last build so that
  nothing goes missing unnoticed.
- **The home page** is `Notebook.kame` rendered as `index.html`, and it always publishes.
- **The theme** is the standard one built into `kame-site`, with a `theme/` folder in the notebook overriding it when
  wanted, so a notebook starts with no theme files at all.
- **Output** goes outside the notebook, under the user's cache folder, so that Syncthing doesn't carry it.
- **A link to something unmarked** shows as plain text, and the build lists it, so nothing private leaks and no link
  goes dead.
- **What doesn't change.** `site.yaml` sites and your existing essays work as they do today. RSS, dates, images, drafts
  and deterministic output all reuse what's there. Unmarked documents are simply never read for the site's content.

## 10. Stages

1. `kame-notebook`: finding a notebook, the `.knb`, listing documents. The status row shows the notebook's name.
2. Ctrl-L.
3. Creating a document by following a link.
4. The browser.
5. Search.
6. What links here.
7. Publishing.

Each is its own commit, tried in the app before it lands, and useful without the ones after it. Tests use made-up
notebooks in scratch folders. The riskiest things to pin are the rule that only an untouched new document is ever
removed, and that a link can't create anything outside the notebook, including through `..` and symbolic links.

**Not in the first version:** a command palette, Markdown import and export, a graph, asking an AI about the notebook,
an index on disk, and links that stay intact when a document is moved or renamed. The last is the one most likely to be
missed, and it could follow from having the link data in memory.

## 11. Decisions made on your behalf

- **No index on disk,** since this scale doesn't need one.
- **The jump list only creates a document when you pick that row,** never from Enter on a near match.
- **Search covers margin notes and footnotes, but not private notes,** matching spell check. Say if you'd rather find
  your own notes too, which seems reasonable for search.
- **Links to a missing document always create `.kame`,** never Markdown.
- **`Ctrl-\` for the browser.**
- **No `url` means a site can still be built,** with links relative to the site, and it simply can't produce a full RSS feed
  until one is set.

## 12. For you to settle

1. **The search key.** Ctrl-Shift-S is already Snapshot Now, and Ctrl-Shift-F, the natural partner of Ctrl-F, already
   adds a footnote. Either Snapshot moves to make room for your choice, or search takes another key.
2. **The home page.** You said `Notebook.kame` first and then that `Notebook.md` renders as `index.html`. This design
   assumes `Notebook.kame`. Allowing either is simple if you meant that, with `.kame` winning if both exist.
3. **`.knb` or `notebook.yaml`.** You said the `.knb` file is the marker and then mentioned `notebook.yaml`. This design
   assumes a YAML file named like `Philosophy.knb`, so that the app can recognize it and be its file type.
