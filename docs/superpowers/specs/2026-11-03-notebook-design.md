# Kame notebook — design

Date: 2026-11-03
Status: for review, revised after four rounds of answers. Nothing is built. Section 13 says what's left, section 12 ends with the decisions made on your behalf, and section 11 says how easily each decision can be undone. The
starting notes are in `MultiKame.kame`.

## 1. Purpose

A notebook is a focused collection of writing and research: tens or hundreds of documents on one subject or
project, with the papers and images they draw on, kept in one folder, that can be moved through quickly while writing
and published as its own site. There will be several. kame stays a page with very little around it, and everything here
appears only when asked for.

It isn't a replacement for Obsidian, a place for all notes, or a Markdown editor.

## 2. What's been decided

| Question | Decision |
|---|---|
| How to decide | prefer what can easily be undone if it turns out wrong |
| What it's for | a focused collection, not all notes |
| How many | several, one open at a time. An über-notebook is possible someday and isn't designed for |
| The app | a superset of kame, not a separate program |
| What marks a notebook | a `notebook.yaml` file in its root |
| Index documents | `Notebook.kame`, always that name. In the root it's the contents page and the site's home page, and in any other folder it's that folder's index |
| Stored state | none beyond `notebook.yaml`. No index and no database, so that nothing derived can go stale |
| Search | the same matches as Ctrl-F, across the notebook, including private notes |
| Keys | Ctrl-L to jump, Ctrl-R to search, Ctrl-Shift-O for the browser |
| Papers and images | under `static/`, which is public and isn't searched |
| Markdown | minimal, page-like, edited in an external editor such as kge. Import and export are left for later |
| On screen | nothing until asked. A jump list, a search and a hideable browser |
| Publishing | each notebook is its own site, and only marked documents publish |
| Following a link to a missing document | creates it, only inside the notebook |
| What links here | shown at the bottom of the document |
| Moving a document | placement is fixed for now. A move command that repairs links comes in a later revision |
| Syncing | set by the sync design, on which this depends in the ways section 10 lists |

## 3. A notebook on disk

```text
philosophy/
├── notebook.yaml
├── Notebook.kame        the contents page, and the site's home page
├── about.md
├── maritain/
│   └── materialism.kame
├── static/              papers and images, all of it public
│   └── maritain-1953.pdf
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

**What counts as a document.** `.kame` and `.md` files under the root, and a `X.kame.sync/` folder with no `X.kame` beside it,
which is a document with only logs, as on a new machine. Hidden folders, `static/`, `theme/`, conflict copies and retired
`.merged` files are left out.

## 4. The code, and why there's no index

A new crate, `kame-notebook`, with no GUI, on `kame-core`, in keeping with the layering. It finds a notebook, reads
its `notebook.yaml`, lists its documents, searches them, and works out what links where. The app and `kame-site` both
use it.

**Reading the files is enough at this scale.** A background thread opens one document at a time with
`Db::open_read_only`, reads only its head row, and moves on, so a document's history is never read and nothing is
written. Only the list of paths and titles stays in memory. Measured on Talos, with 1,000 documents of 15 KB of text
each, about as much as the largest real one:

| Option | Same as Ctrl-F | Extra disk | Typical search | First search, cold | Peak memory |
|---|---|---:|---:|---:|---:|
| **Reading the files** | yes, by using its matcher | 0 | 84 ms | 505 ms | 5.5 MB |
| Plain text table | yes, by using its matcher | 17 MB | 47 ms | 59 ms | 8 MB |
| Trigram index, checked by the matcher | yes, in 23 test queries | 71 MB | under 1 ms | 267 ms | 8 MB |
| Word index | no: misses mid-word matches, dashes and single letters | 40 MB | under 1 ms | 9 ms | 9 MB |
| Everything in memory | yes, by using its matcher | 0 | 41 ms | not measured | 28 MB |

At 100 documents, reading the files takes 8.5 ms, or 51 ms cold. The text was sampled from real writing but made up, and
the file timings used one ASCII search. Searches for non-ASCII text ran about ten times slower through a text table, due
to the matcher allocating for each character, and a quick change brought that to about 200 ms. It should be fixed when
the matcher moves.

**If this proves too slow,** a trigram index can go in behind the same search function, kept in each machine's cache
folder and not in the notebook. Nothing in a document or in `notebook.yaml` would change, and deleting it would go back
to this. The likely reasons would be a notebook of several thousand documents, or wanting to search inside papers, whose
text is too slow to extract on every search.

## 5. Jumping: Ctrl-L

A small list opens over the page. Typing narrows it by title and path, with letters needing only to appear in order, and
recently opened documents come first. Enter opens the choice and Escape closes the list, as with the search bar. If
nothing matches, the last row offers to create a document at the typed path, so nothing is created by accident.

## 6. Searching: Ctrl-R

The same kind of list, searching the text of every document in the notebook, including margin notes, footnotes and
private notes.

**It matches as Ctrl-F does.** Any run of characters, ignoring case and keeping accents, within one block. `find_matches`
moves from the app into `kame-core`, so that the two can't drift apart, and it's extended to margin notes, footnotes and
private notes for this list. The open document is searched as it stands on screen.

**Results arrive as they're found,** and a new keystroke cancels the search under way. Every match is listed, with the count at the
top: searching for `the` gets every `the`, and choosing what to search for is up to the writer. Rows come by document, in the order
they're read. Each shows the document and the line around the match, and choosing one opens that document at the match, with Ctrl-[
going back. The list draws only the rows in view, so its length costs little beyond the rows' text.

Search doesn't look in `static/`. A short `.kame` of notes on a paper, anywhere else, is searched like anything else.

## 7. The browser: Ctrl-Shift-O

A folder tree of the notebook on the left, with the current document highlighted. Clicking a document opens it, and
clicking a paper or image opens it in the system's viewer.

- **Hidden by default.** The key toggles it, as does an item in the View menu, and hide-chrome hides it too. It remembers
  whether it was open and which folders were expanded, in the app's config.
- **It takes the left margin.** The document stays exactly where it is, so nothing rewraps, as long as the text and the
  right margin still fit. If they don't, the document hides and only the browser shows. Choosing a document or closing the
  browser brings the page back.
- **Only in a notebook.**

## 8. Links

**A missing document.** Following a relative link to a document that doesn't exist creates it, as a `.kame` file, along
with any folders it needs, and opens it. Ctrl-[ comes back. Four rules keep this safe:

- **Only inside the notebook.** A link leading outside it, including through `..` or a symbolic link, says the link has
  rotted, as does any link outside a notebook.
- **Only for what could be a document.** The target must have no extension or end in `.kame`, or be a folder, which
  makes that folder's `Notebook.kame`. A missing `static/paper.pdf` says the link has rotted, and never makes
  `paper.pdf.kame`.
- **Mistyped links leave nothing behind.** If you leave a new document without writing anything, it's removed, with its
  `-wal` and `-shm`. Only a document created this way, this session, and never edited is ever removed. It writes no logs until
  it's edited, so there's nothing to take back from another machine.
- **A heading in the link** becomes the new document's first heading, so that the link then leads somewhere.

**A link to a folder.** It opens the folder's `Notebook.kame`.

**A link to a paper or image.** Inside a notebook, following a relative link to a file that isn't a document opens it in
the system's viewer. Today, kame would try to open it as a document.

**What links here.** Below the footnotes, in the apparatus type, a short list of the documents linking to this one. A
click opens one. It isn't part of the document, so it never reaches exports or the site, and it isn't shown when
empty. It comes from a background read of the notebook when a document opens, so it can take a moment to appear.

**Moving a document.** Placement is fixed for now. kame offers no way to move a document, and one moved by hand keeps
its new place while links to the old one go stale. In a folder with logs, section 10 says what a moved document leaves behind. Following such a link would make a new, empty document at the old
path, which is removed again if left unwritten. A later revision adds a Move command through the command palette, which
would read the notebook for every document linking to the old path, and rewrite those links as ordinary edits at the
moment of the move.

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
- **Index pages.** `Notebook.kame` in the root becomes the home page and always publishes. In another folder it becomes
  that folder's `index.html`, if marked like any other document. A folder without one simply has no page, and a link to it
  publishes as written, leaving the web server to answer.
- **The theme** is the standard one built into `kame-site`, with a `theme/` folder in the notebook overriding it when
  wanted, so a notebook starts with no theme files at all.
- **Output** goes outside the notebook, under the user's cache folder, so that Syncthing doesn't carry it.
- **A link to an unmarked document** shows as plain text, and the build lists it, so nothing private leaks and no link
  goes dead.
- **What doesn't change.** `site.yaml` sites and your existing essays work as they do today. RSS, dates, images, drafts
  and deterministic output all reuse what's there. Unmarked documents are simply never read for the site's content.

**`static/` is public, and all of it publishes,** at `/static/`, so that its URLs follow the folders too. That differs from a
`site.yaml` site, which copies the contents of its `static/` to the site's root.

- **Links into it are rewritten.** A page at `/maritain/materialism/` is further from `static/` than its document is, so a
  relative link to a paper would break unless the build rewrites it.
- **A link to a file that isn't there** publishes as written, leaving the web server to answer, and the build lists it.
- **Conflict copies don't publish.** Syncthing leaves them in any folder, and `static/` is copied whole.
- **Anything not for publication** goes in any other folder, such as `papers/`. The browser and links still reach it, and a
  link to it from a published document shows as plain text, as with an unmarked document.
- **Files the site itself needs,** such as fonts and styles, go in `theme/static/`, and publish to the site's root as usual.

## 10. Syncing

The notebook works with plain Syncthing, or with revision logs if its folder has `.kame-logs`. These are what logs mean for it.

**Reading.** A document with a database is read from its head row. One with only logs is read from them, at about 3 ms a document,
and its database is built when it's opened.

**What's on this machine is what's searched and published.** kame syncs only the open document, so a closed one can be behind what
another machine wrote until it's next opened. Search, what links here and `kame-site` read what this machine has. This is accepted
until there's a sync server made for kame: it's a matter of which machine you work from, and a background pass merging into documents
nobody is looking at, as section 13 of `2026-11-03-sync-design.md` suggested, isn't worth what it adds.

**Which files are in a notebook isn't kept in a list.** Tried on `notebook-test`: renaming a document and its logs on Talos leaves Titan
with its old database at the old path and the logs at the new path, as Syncthing carries the logs and not the database. So Titan
would list a stale document beside the moved one. A list of a notebook's files would have to be written by every machine, which is the
conflict logs exist to avoid, and it could disagree with the folder. The documents can already say what happened:

- A database that has synced before records its last revision, so one with no logs beside it had logs that went somewhere.
- If logs elsewhere in the notebook hold the same document's id, it moved, and the database is renamed to join them, keeping its undo
  history and snapshots.
- If none do, it was deleted elsewhere, and the database is renamed with `.retired` added, not deleted.

A database that has never synced has simply not written logs yet, and is left alone. This belongs with the Move command, and a list can
still be added later if this falls short.

**Deleting.** A document in a folder with logs is deleted by removing both `X.kame` and `X.kame.sync/`. Removing only the database
gets it rebuilt from its logs on the next open, as the live test showed.

**Timing.** Writing on one machine and then going to another takes minutes, and a sentence arrives in about 25 seconds, so a link followed
to a document that hasn't arrived yet isn't designed for.

## 11. If this turns out wrong

| Decision | How to go back |
|---|---|
| No index | easily. Add one behind the same function, in the cache folder. No document changes |
| Keys | easily. A few lines, and some relearning |
| `notebook.yaml` and its name | easily. Move the settings to something else and read both for a while |
| Search matches and private notes | easily. Nothing is stored |
| Accepting that closed documents can be behind | easily. A background pass can be added, or a sync server |
| No list of a notebook's files | easily. A list can be added later, as nothing depends on its absence |
| Creating on following a link | easily. Turn it off. Documents already made are ordinary documents |
| The browser | easily. Nothing is stored but a preference |
| *Publish* mark in `Metadata` | fairly easily. A field in every marked document that can simply stop being read |
| Relative links between documents | with work. They're in the text of documents, but the format exists already, and a scan can rewrite them |
| **URLs that follow folders** | **hard.** Once a site is public, others link to its URLs. Redirects could cover a later move |
| Everything in `static/` being public | easily. Change the rule. A file published by mistake is removed from the server and the site republished |

## 12. Stages

1. `kame-notebook`: finding a notebook, `notebook.yaml`, listing documents. The status row shows the notebook's name.
2. Ctrl-L.
3. Creating a document by following a link.
4. The browser, and opening papers and images in the system's viewer.
5. Search.
6. What links here.
7. Publishing.

Each is its own commit, tried in the app before it lands, and useful without the ones after it. Tests use made-up
notebooks in scratch folders. The riskiest things to pin are the rule that only an untouched new document is ever
removed, that a link can't create anything outside the notebook, that search and what links here never write anything
beside a document, and that a build publishes nothing from outside `static/` but marked documents and the images they show.

**Not in the first version:** a command palette, moving a document with its links repaired, an index, searching `static/`, Markdown import and export, a graph, and asking an AI about the notebook.

**Decisions made on your behalf.**

- **The jump list only creates a document when you pick that row,** never from Enter on a near match.
- **Links to a missing document always create `.kame`,** never Markdown.
- **The file is always called `notebook.yaml`,** as with `site.yaml`, rather than being named after the notebook. So there
  can't be two in a folder.
- **No `url` means a site can still be built,** with links relative to the site, and it simply can't produce a full RSS feed
  until one is set.

## 13. What's left

Nothing is left to settle in this design on its own. Section 13 of the sync design still describes a background pass, and needs
updating when this lands. The implementation plan waits on your review of this.
