# Kame notebook — design

Date: 2026-11-03
Status: draft for review, revised after your first answers. Nothing is built. Section 12 lists what's left for you to
settle, and section 11 the decisions made on your behalf. The starting notes are in `MultiKame.kame`.

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
| What marks a notebook | a `notebook.yaml` file in its root |
| Contents page | `Notebook.kame` in the root, which is also the site's home page |
| The index | a SQLite database on disk, outside the notebook (see section 12) |
| Markdown | minimal, page-like, edited in an external editor such as kge. Import and export are left for later |
| On screen | nothing until asked. A jump list, a search and a hideable browser |
| Publishing | each notebook is its own site, and only marked documents publish |
| Following a link to a missing document | creates it, only inside the notebook |
| What links here | shown at the bottom of the document |
| Moving a document | placement is fixed for now. A move command that repairs links comes in a later revision |

## 3. A notebook on disk

```text
philosophy/
├── notebook.yaml
├── Notebook.kame        the contents page, and the site's home page
├── about.md
├── maritain/
│   └── materialism.kame
└── theme/               optional, for a site that differs from the standard theme
```

`notebook.yaml` holds only what can't be worked out from the folder:

```yaml
title: Philosophy
url: https://philosophy.example.net/
author: K. Isom
remote: kyle@host:/srv/philosophy/
```

`title` defaults to the folder's name if left out. `url`, `author` and `remote` are only for
publishing. `remote` is the same idea as `UPSTREAM` in a `kame-site` Makefile. Unknown keys are refused, as in
`site.yaml`, so a typo shows up instead of quietly doing nothing.

**Finding the notebook.** Opening any document looks up through its parent folders for a `notebook.yaml`, and the
nearest one wins. So there's no separate step to enter a notebook, and a notebook inside another is just its own
notebook. `kame philosophy/` opens its `Notebook.kame`, or the browser if there isn't one.

**What counts as a document.** `.kame` and `.md` files under the root, leaving out hidden folders, `theme/`,
conflict copies and retired `.merged` files.

## 4. The code and the index

A new crate, `kame-notebook`, with no GUI, on `kame-core`, in keeping with the layering. It finds a notebook, reads
its `notebook.yaml`, lists its documents, keeps the index, searches it, and works out what links where. The app and
`kame-site` both use it. It adds no dependency: the SQLite that kame already bundles has full-text search (FTS5)
compiled in.

**Where the index lives.** In the user's cache folder, one database per notebook, named from a hash of the notebook's
full path. It stays out of the notebook because Syncthing would otherwise carry a live database between machines, and
two machines writing it is the conflict-copy problem that `kame-sync` exists for. Each machine keeps its own. Everything
in it comes from the documents, so it can be deleted at any time and is rebuilt, which is also what happens when
its schema version doesn't match.

**What it holds.**

- Each document's path, modified time, size, title and publish mark.
- One full-text row per block, margin note and footnote, so that a match leads to its place in the document.
- Each link: which document it's in, the target as written, and the document it leads to.

**Keeping it current.** A background thread walks the folder when a notebook opens and whenever the jump list or search
opens, comparing modified time and size, and rereads only what changed. It opens one document at a time with
`Db::open_read_only`, reads only the head row and never changes a document. The open document's rows are refreshed
before a search or a list of what links here.

**What it costs in memory.** Only the list of paths and titles for the browser and jump list stays in memory. Search
results come from queries. A document's history, which is most of its file, is never read. Measured on three real
documents:

| | File | History | Head row | Searchable text |
|---|---:|---:|---:|---:|
| Largest | 815 KB | 675 KB | 22 KB | 15 KB |
| Second | 397 KB | 307 KB | 9 KB | 4 KB |
| Third | 356 KB | 279 KB | 11 KB | 8 KB |

If the index can't be opened or written, the status row says so, and search and what links here are unavailable.
Writing and every other part of kame carry on without it.

## 5. Jumping: Ctrl-L

A small list opens over the page. Typing narrows it by title and path, with letters needing only to appear in order, and
recently opened documents come first. Enter opens the choice and Escape closes the list, as with the search bar. If
nothing matches, the last row offers to create a document at the typed path, so nothing is created by accident.

## 6. Searching: Ctrl-Alt-F

Cmd-Option-F on a Mac, which stage 5 has to try there. The same kind of list, searching the text of every document in the
notebook, including margin notes and footnotes.

It finds words and the beginnings of words, ignoring case and accents, and quotation marks ask for an exact phrase.
The best matches come first. That differs from Ctrl-F, which finds any run of letters, so `terial` finds nothing
here though it would on the page. Each row shows the document and the line around the match, and choosing one opens
that document at the match, with Ctrl-[ going back.

## 7. The browser: Ctrl-Shift-L

A folder tree of the notebook's documents on the left, with the current one highlighted. Clicking a document opens it.

- **Hidden by default.** The key toggles it, as does an item in the View menu, and hide-chrome hides it too. It remembers
  whether it was open and which folders were expanded, in the app's config.
- **It takes the left margin.** The document stays exactly where it is, so nothing rewraps, as long as the text and the
  right margin still fit. If they don't, the document hides and only the browser shows. Choosing a document or closing the
  browser brings the page back.
- **Only in a notebook.**

## 8. Links

**A missing document.** Following a relative link to a document that doesn't exist creates it, as a `.kame` file, along
with any folders it needs, and opens it. Ctrl-[ comes back. Three rules keep this safe:

- **Only inside the notebook.** A link leading outside it, including through `..` or a symbolic link, says the link has
  rotted, as does any link outside a notebook.
- **Mistyped links leave nothing behind.** If you leave a new document without writing anything, it's removed, with its
  `-wal` and `-shm`. Only a document created this way, this session, and never edited is ever removed.
- **A heading in the link** becomes the new document's first heading, so that the link then leads somewhere.

**What links here.** Below the footnotes, in the apparatus type, a short list of the documents linking to this one. A
click opens one. It isn't part of the document, so it never reaches exports or the site, and it isn't shown when
empty. It comes from the index, so a link that just arrived from another machine can take a moment to appear.

**Moving a document.** Placement is fixed for now. kame offers no way to move a document, and one moved by hand keeps
its new place while links to the old one go stale. Following such a link would make a new, empty document at the old
path, which is removed again if left unwritten. A later revision adds a Move command through the command palette, which
would use the index to find every document linking to the old path and rewrite those links as ordinary edits at the
moment of the move. The index records links so that this needs no change to its design.

## 9. Publishing

Each notebook is its own site. `kame-site` learns to build one, from inside its folder or by naming it:

```sh
kame-site build ~/notes/philosophy
kame-site sync ~/notes/philosophy    # builds, then sends it to `remote`
```

- **What's marked.** A *Publish* tick in Document Properties, stored in `Metadata` like tags. In Markdown it's
  `publish: true` in the front matter. It's off by default, since notebooks hold research and working notes too.
- **URLs follow the folders.** `maritain/materialism.kame` becomes `/maritain/materialism/`, which differs from `/k/` on
  your essays site.
- **The home page** is `Notebook.kame` rendered as `index.html`, and it always publishes.
- **The theme** is the standard one built into `kame-site`, with a `theme/` folder in the notebook overriding it when
  wanted, so a notebook starts with no theme files at all.
- **Output** goes outside the notebook, under the user's cache folder, so that Syncthing doesn't carry it.
- **A link to something unmarked** shows as plain text, and the build lists it, so nothing private leaks and no link
  goes dead.
- **What doesn't change.** `site.yaml` sites and your existing essays work as they do today. RSS, dates, images, drafts
  and deterministic output all reuse what's there. Unmarked documents are simply never read for the site's content.

The build reads the documents themselves, not the index, so that a site never depends on one machine's cache.

## 10. Stages

1. `kame-notebook`: finding a notebook, `notebook.yaml`, listing documents. The status row shows the notebook's name.
2. The index, with paths and titles, and Ctrl-L.
3. Creating a document by following a link.
4. The browser.
5. Full-text rows and search.
6. Link rows and what links here.
7. Publishing.

Each is its own commit, tried in the app before it lands, and useful without the ones after it. Tests use made-up
notebooks in scratch folders. The riskiest things to pin are the rule that only an untouched new document is ever
removed, that a link can't create anything outside the notebook, and that the index never writes to a document or
inside the notebook.

**Not in the first version:** a command palette, moving a document with its links repaired, Markdown import and export,
a graph, and asking an AI about the notebook.

## 11. Decisions made on your behalf

- **The jump list only creates a document when you pick that row,** never from Enter on a near match.
- **Links to a missing document always create `.kame`,** never Markdown.
- **The file is always called `notebook.yaml`,** as with `site.yaml`, rather than being named after the notebook. So there
  can't be two in a folder.
- **Search finds whole words and their beginnings,** not any run of letters. That's what makes it fast and lets it rank
  results, and full-text search can be switched to any run of letters later at the cost of a larger index.
- **No `url` means a site can still be built,** with links relative to the site, and it simply can't produce a full RSS feed
  until one is set.

## 12. For you to settle

1. **The index.** This design has `notebook.yaml` for what you edit by hand and a SQLite index in each machine's cache
   folder. The alternative is a single SQLite notebook file in the folder, which Syncthing would carry and could
   conflict over.
2. **The search key.** Ctrl-Alt-F is free and keeps the F, and Ctrl-Shift-F stays the footnote. The other way is to give
   search Ctrl-Shift-F and find the footnote a new key, which would break its pairing with Ctrl-Shift-N.
3. **The browser key.** Ctrl-Shift-L sits beside Ctrl-L, on the home row, and the two both get around the notebook.
   Ctrl-Shift-O, after Ctrl-O for open, is the other free choice.
4. **Private notes in search.** They're left out for now, matching spell check. With the index only on your own machines,
   including them would be safe, and you might well want to find your own notes.
