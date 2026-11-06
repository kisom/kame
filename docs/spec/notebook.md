# The kame notebook

As built at commit `aa493c7`. A notebook is a folder of documents with a settings file in it, which can be moved through, searched and published. This describes what it is on disk and the rules a program must follow to agree with kame. Keystrokes and screen layouts are left to `docs/guide.md`, and the reasons behind the design to `docs/superpowers/specs/2026-11-03-notebook-design.md`.

Documents are in [document-format.md](document-format.md), and revision logs are in [merging.md](merging.md).

**Nothing derived is stored.** A notebook has no index, database or list of its files. Everything is worked out from the folder, one document at a time, and working it out changes nothing. A notebook is still an ordinary folder if every program but a file manager goes away.

## 1. On disk

```text
garden/
├── notebook.yaml        marks this folder as a notebook
├── Notebook.kame        the contents page, and the site's home page
├── about.md
├── maritain/
│   ├── Notebook.kame    this folder's index, if it wants one
│   ├── materialism.kame
│   └── .materialism.kame.sync/   its revision logs, if logs are on
├── papers/              private files, unless a published document shows one as an image
├── images/              pictures, private until a published document shows them
├── references/          the sources documents cite, one KEY.yaml each
├── static/              public: all of it publishes
├── theme/               changes to how the site looks
└── output/              the built site
```

### 1.1 `notebook.yaml`

A folder is a notebook if it has a file with this exact name. All keys are optional:

```yaml
title: The Garden
url: https://example.net/
author: Your Name
remote: you@example.net:/srv/garden/
```

| Key | Meaning |
|---|---|
| `title` | What the notebook is called. If it's absent or blank, the folder's name, and failing that `notebook` |
| `url` | The site's address. A trailing `/` is ignored |
| `author` | The site's author, used for any published document with none of its own |
| `remote` | Where `kame-site sync` sends the site, as rsync takes it |

An unknown key must be refused, not ignored, so that a typo can't quietly do nothing. A file that's empty, or holds only blank lines and `#` comments, is valid and sets nothing.

### 1.2 Finding a notebook

The notebook holding a path is the nearest folder with a `notebook.yaml`, going up from that path. The root is the folder's real path, with symbolic links resolved.

**A notebook inside a notebook is another notebook.** A folder below the root with its own `notebook.yaml` belongs to that notebook alone, and is no more part of the outer one than a folder outside it is. A `notebook.yaml` deeper down, as in `a/b/`, cuts off only `a/b`, not `a`. The outer notebook doesn't list, search, publish or settle anything in it, and a path in it isn't inside the outer notebook for links (section 4), images (section 5) or moves. The inner notebook is as it would be anywhere.

A new notebook writes `Notebook.kame` first, unless one is there, then `notebook.yaml` with its keys commented out. That file goes last so that a failure leaves the folder not a notebook. A folder that already has a `notebook.yaml` is refused and left alone.

`Notebook.kame` is the contents page. A new one holds only a level 1 heading with the folder's name, and the rest is for the person to write. In any folder, a file with that name is the folder's index.

A `notebook.yaml` that can't be read is reported as an error for that notebook, and the search doesn't carry on to folders further up. The app then works with no notebook.

## 2. What's in a notebook

Walk the folder from the root, and sort the result by path from the root, with `/` between folders, comparing bytes. Don't follow symbolic links, so nothing outside is listed and a loop can't catch the walk.

Each entry is either a **document**, which kame opens and search reads, or a **file**, which opens in the system's viewer.

**Skip these files:**

- Anything starting with `.`.
- Anything ending in `-wal`, `-shm`, `.merged`, `.retired` or `.building`.
- Anything containing `.sync-conflict-`.
- `notebook.yaml` in the root.
- A `.yaml` file directly in `references/` in the root. It is a reference (section 2.1), not an entry. Anything else there, including documents, files and deeper folders, is an ordinary entry, and a `references/` folder elsewhere is an ordinary folder.

**Skip these folders:**

- `output` in the root.
- Anything else starting with `.`, except as below.
- Any folder holding a `notebook.yaml`, which is another notebook (section 1.2), wherever it is.

**Log folders.** A folder named `.NAME.sync`, where `NAME` ends in `.kame` in any case, stands for a document called `NAME` if no such file is beside it. This is a document with only logs, as on a new machine. It's listed as a document, unless it's inside `static`, `images` or `theme` in the root. Its contents are never listed.

**Documents.** A file is a document if its name ends in `.kame`, `.md` or `.markdown`, in any case. Anything else is a file.

Folders aren't entries. A browser derives them from the entries' paths, so an empty folder never appears.

**Folders holding no documents.** Under `static`, `images` and `theme` in the root, everything is a file, whatever its name. That applies only in the root. A `static` folder deeper down is an ordinary folder.

### 2.1 References

A notebook keeps one file per reference, `references/KEY.yaml` under its root. The key is the file name and isn't repeated inside the file, so the two can't disagree. A key is ASCII letters, digits, `-` and `_`, starting with a letter, at most 64 bytes. A document cites a reference by typing `[KEY]`.

```yaml
kind: article
authors: [D. E. Knuth, M. F. Plass]
title: Breaking paragraphs into lines
published_in: "Softw.: Pract. Exper."
volume: "11"
number: "11"
pages: 1119–1184
month: Nov.
year: "1981"
```

| `kind` | Fields |
|---|---|
| `article` | `authors`, `title`, `published_in`, `volume`, `number`, `pages`, `month`, `year`, `doi`, `url` |
| `conference` | `authors`, `title`, `published_in`, `location`, `month`, `year`, `pages`, `doi`, `url` |
| `book` | `authors`, `editors`, `chapter`, `title`, `edition`, `place`, `publisher`, `year`, `pages`, `url` |
| `web` | `authors`, `title`, `site`, `accessed`, `url` |
| `freeform` | `text` |

All are optional except `title` (or `text`). All are strings, or lists of strings for names; a bare number is read as text. References have no field for private remarks, so nothing private can publish along with one.

An unknown field or kind is refused rather than ignored, as in `notebook.yaml`. A file that can't be read is reported by name, the others keep working, and kame doesn't overwrite or remove it. Of two files whose keys differ only in case, the first by name is used and the other is reported, and a key that differs only in case from an existing one can't be saved. Hidden files, conflict copies, `.building` files and symbolic links in the folder are skipped.

Writes go to a hidden temporary file and are renamed into place. Reference edits aren't undo steps, the same as a document's metadata. Nothing derived is stored: how many documents cite a reference is worked out by reading them.

## 3. Reading documents

Reading a document for a list, a search or a site must change nothing:

| The path holds | Read it |
|---|---|
| A file ending in `.md` or `.markdown`, in any case | Import the Markdown in memory, using the document's folder as the base and its stem as the fallback title |
| Any other file | As a read-only `.kame` ([document format, section 1.2](document-format.md)) |
| No file, but a log folder | From the logs: the document and metadata of the first head, in sorted order ([merging](merging.md)) |

A document that can't be read stays in the list under its file name. Search and the links list skip it with no message, and publishing skips it with a warning.

Search reads what's on the screen in place of the file for the open document, since that can be ahead of the disk. The links list reads files only, and is worked out when a document opens, not as it's edited.

Opening a Markdown document is an import. The result has no file of its own and the original is never written. The first save asks where the `.kame` file goes, and until then links resolve against the Markdown file.

### 3.1 Titles

In lists and search, a document goes by the first of these that isn't blank: `metadata.title`, then `document.title`, then its file name up to the first `.`. A site has its own rule (section 7.2).

## 4. Links

A document link's `target` is `PATH#HEADING`, as in the [document format](document-format.md). Inside a notebook, following one has four outcomes:

| Outcome | Meaning |
|---|---|
| **Open** | A document to open |
| **Create** | A document that isn't there yet and may be made |
| **View** | Some other file, for the system's viewer |
| **Rotted** | Not in this notebook: the link is broken, or leads outside |

Publishing treats **Rotted** as nowhere. The desktop app then tries the plain lookup in [document format, section 2.4](document-format.md), so a link to a document that exists outside the notebook still opens. Only if that fails does it say there's no document there. Other implementations should decide this deliberately, since it lets a link leave the notebook.

**Before these rules:** an empty `PATH` with a heading moves within the current document, and says so if there's no such heading.

Paths are taken literally and never percent-decoded, so `a%20b.md` doesn't name `a b.md`.

The rules, in order:

1. If `PATH` is empty, starts with `#` or `//`, or has a scheme, the link has rotted. A scheme is a letter, then letters, digits, `+`, `-` or `.`, up to a first `:`, so a colon later in a path doesn't make one. An absolute path isn't excluded here, and rule 3 judges it.
2. Trim surrounding whitespace, expand a leading `~/` or a bare `~`, resolve against the linking document's folder, and take out `.` and `..` by what they say. What's named may not exist yet. The linking document's path must already be in terms of the notebook's real root, or every link rots at rule 3 (section 4.3).
3. If the result isn't inside the notebook, the link has rotted. A path in another notebook below the root isn't inside (section 1.2). That's judged first by the path as written, then by the real location of as much of the path as exists, so that a symbolic link can't lead out. A dangling link counts as existing, and has no real location, so it gives **Rotted**. Rules 4 and 7 to 9 add to the path, and the result must pass the same check.
4. If it's a folder, or `PATH` ends in `/`, look at its `Notebook.kame`: **Open** if it's there or has a log folder, **Create** if it's in the documents area, otherwise **Rotted**.
5. If it's a file: **Open** if it's in the documents area and ends in `.kame`, `.md` or `.markdown`, otherwise **View**.
6. If nothing is there and it isn't in the documents area, it has **Rotted**.
7. Try adding `.kame`, then `.md`: **Open** if that file exists. `.markdown` isn't tried, so a link to one has to name it.
8. If it has no extension, add `.kame`. If its extension is `.kame` in any case, keep it. With any other extension it has **Rotted**, so a missing `paper.pdf` never makes `paper.pdf.kame`.
9. **Open** if that document has a log folder, since it exists and only needs building. Otherwise **Create**.

**The documents area** is everything inside the notebook except what's under `static`, `images`, `theme` or `output` in the root, and anything with a part starting with `.`.

### 4.1 Creating a document

Creating makes any folders needed, then a `.kame` file with a new document titled from the file's stem. If the link named a heading, the first block becomes a level 1 heading with that text, so the link now leads somewhere. None of this is an edit, so it writes no ops.

Following a link to **Create** makes the document at once and opens it, remembering which folders it made. If it's then left or the app exits, in the same session, with no edits and its metadata unchanged since it opened, it's removed with its `-wal` and `-shm`, along with those folders if they're empty. **That's the only case where kame deletes a document.** While it's untouched it writes no revision logs, so nothing has to be taken back from another machine.

The jump list can also create a document. When nothing matches and the typed text isn't blank, that text goes through section 4 as a link from the root's `Notebook.kame`, and a row is offered only if the outcome is **Create**. A document made this way is kept even if it's left untouched.

### 4.2 What links here

A document's "linked from" list is every other document holding a `DocumentLink` span whose target follows to **Open** on it. It's only drawn on screen and is never stored, exported or published. Links in `Link` spans don't count, and importing Markdown only makes `Link` spans, so Markdown documents never appear in the list.

### 4.3 Other links, and what to write

The rules above cover document links. In the app, inside a notebook, a `Link` span or image link holding a relative URL follows the same rules. A `Link` span or image link holding `#x` goes to that heading, in or out of a notebook. Any other URL goes to the system's opener, unless it's a script URL or starts with `-`.

**New links should be relative.** The desktop's link dialog writes `~/…` or a whole path, and those rot on any machine where the notebook lives somewhere else. Typing `[[path]]` keeps what was typed.

**Use real paths.** Comparing with the root starts with the path as written, so a document opened through a symbolic link would make its links rot. When a document's path isn't inside the root as written, resolve its folder first, as the desktop does. This matters on Android, where `/sdcard` is a symbolic link, and on macOS, where `/tmp` and `/var` are.

## 5. Images

An image block inside a notebook stores a relative path from the document's folder, with `/` separators. That holds on another machine or after the notebook moves. An image outside the notebook keeps its whole path.

Inserting an image from outside copies it into the notebook first, at `images/PATH/STEM/NAME` for a document at `PATH/STEM.kame`, so `maritain/essay.kame` gets `images/maritain/essay/plate.jpg`:

1. If a file with that name is there with the same bytes, reuse it.
2. If it differs, try `plate-2.jpg`, `plate-3.jpg` and so on. Nothing already there is replaced.
3. Copy under a hidden name, `.NAME.copying-PID`, and rename into place, so that a failure can't leave half a picture.

An image already inside the notebook isn't copied.

## 6. Search

Search reads every document in list order and reports each match, in blocks first and then in annotation bodies of every kind, including private notes. A match in a note leads to the block and offset where the note hangs, or offset 0 for a whole-block anchor. Each result shows its line, with up to 40 Unicode characters either side.

Matching is the same as find on the page. It compares a window of as many Unicode characters as the search text has, one character at a time, and reports overlapping matches. Two ASCII characters match by ignoring ASCII case. Others match if their full Unicode lowercase forms are equal, so `İ` expands and `ß` doesn't match `SS`, and text isn't normalized. Kotlin's `lowercaseChar` won't give the same answers, because it maps one UTF-16 unit to one.

All offsets are bytes in the original text, even where a character changes length when lowercased. An empty search finds nothing. A search can be stopped between documents.

Documents are read in list order, blocks in document order, and notes in the order of the `annotations` array. A result's line ends at a `\n` on each side and is trimmed, and `…` marks a side that was cut short.

The jump list is separate, and matches titles and paths by whether the typed letters appear in order. How it ranks them is a matter for the interface, not this spec.

## 7. Publishing

Each notebook is its own site, built by `kame-site`. A folder holding both `notebook.yaml` and `site.yaml` is treated as a `site.yaml` site.

### 7.1 What publishes

| Thing | Rule |
|---|---|
| `Notebook.kame` in the root | Always publishes, as the home page |
| Any other document | Only if `metadata.publish` is true. In Markdown, that's `publish: true` in the front matter |
| A document that can't be read | Not published, with a warning, because a marked one can't be told from an unmarked one |
| Everything under `static/` | Publishes at `/static/`, whether or not anything links to it, except hidden files and conflict copies |
| An image under `images/` | Publishes at `/static/images/…` only if a published document shows it |
| Any other file in the notebook that a published image block shows | Publishes beside that page, including from `papers/` or `static/` |
| Files under `theme/static/` | Publish to the site's root |
| A reference in `references/` | Only as an entry in the References section of a published document citing it, never as a file |
| The site's own pages | Archive and tag pages, and a feed when `url` is set, generated from what publishes |

Hidden files, hidden folders, conflict copies and symbolic links under `static/` don't publish. Nor does anything in another notebook below the root (section 1.2), even under `static/`, `images/` or `theme/`: its documents aren't listed, a link to one publishes as its words, and an image in one is outside the notebook.

**What goes public is decided by the mark on a document, by `static/`, and by what a published document shows.** An implementation must not publish anything else.

A marked document can stop the build. A date that won't parse is an error, as is a tag with no letters or digits, and so are two things at one address (section 7.2). A document's date is `published`, then `created`, then the file's own time with a warning.

### 7.2 Addresses

A document's address follows its folders: `maritain/materialism.kame` becomes `/maritain/materialism/`. `Notebook.kame` in a folder becomes that folder's address, so `maritain/Notebook.kame` becomes `/maritain/`. Each path part is percent-encoded. With no `Notebook.kame` in the root, the home page is the site's usual listing.

Two things at one address stop the build, such as `a.kame` and `a.md`, or a document with the same name as one of the site's own pages.

On the site, a document's title is the first of `metadata.title` and `document.title` that isn't empty or `Untitled`, then its file's stem, which differs from section 3.1. A first heading repeating the title is left out of the page.

### 7.3 Links on the site

Relative links are rewritten, because a page sits further from everything than its document does. Anything from the first `#` on is dropped, since the site has no heading anchors.

Which links this covers depends on the span:

| Link | Treatment |
|---|---|
| `Link` span or image link holding a web address: `http`, `https`, `mailto`, `ftp`, `ftps`, `tel` or `sms` in any case, or starting with `//` | Left alone |
| `Link` span starting with `#` | Left alone |
| `DocumentLink` starting with `#` | Publishes as its words, with no warning |
| Image link starting with `#` | Loses its link, with a warning |
| Any other `Link` span, `DocumentLink` or image link | The table below. So a `DocumentLink` holding a URL, a `zotero:x` or a site-rooted `/static/x.pdf` publishes as its words, with a warning |


| Following it gives | On the site |
|---|---|
| **Open**, on a published document | That document's address |
| **Open**, on an unpublished one | Its words, with a warning, so nothing private leaks and no link goes dead |
| **View**, under `static/` with no hidden part | That file's address |
| **View**, anywhere else | Its words, with a warning |
| **Create**, of a folder's index | The folder's address, for the web server to answer |
| **Create**, of anything else | Its words, with a warning |
| **Rotted**, on a path under `/static/` inside the notebook with no hidden part | Its site address, tidied and percent-encoded, with a warning, for the web server to answer |
| **Rotted**, otherwise | Its words, with a warning, because naming anything else would say that it exists |

An image block in a published document:

- Under `images/`: its path becomes `/static/images/…`, and the file is copied there.
- Outside the notebook: it's dropped from the page, with a warning.
- A web address (`http://` or `https://`): left as it is.
- Elsewhere in the notebook, including `static/` and `papers/`: the file is copied beside the page, with `-2`, `-3` and so on if names clash, and its `src` becomes that name.
- Missing: left as written, with a warning. So a private path can appear in the page's `src`.

### 7.4 Theme and output

The theme is the one built into `kame-site`, with files in the notebook's `theme/` replacing it one by one. A notebook starts with no theme files at all.

The theme is laid out in a private temporary folder, and pages are worked out in memory. The site is assembled in `.output-building` in the notebook's root, and the old site is moved to `.output-old` before the new one moves in, so that a failure leaves the last good site alone. A build leaves an empty `.kame-site-output` marker in `output/`, and it must refuse to replace a non-empty `output/` without that marker.

In a synced folder, `output/` should be kept out of the sync, or two machines' builds will conflict.

### 7.5 Sending

`kame-site sync` builds, then sends `output/` to `remote` with `rsync -az --delete`, leaving out the marker. Because `--delete` removes whatever else is at `remote`:

- Accept only a plain path, or `[user@]host:path` with a plain host and path, and refuse anything else, including `[host]:path`, `host::module` and `rsync://`. `notebook.yaml` can arrive from another machine, rsync can leave a host that ssh reads as an option, and a remote path goes through the far end's shell.
- Do a dry run first. If it would delete more than 5 files, list up to 20 and stop. Agreeing is to that remote and those files: going ahead must run the dry run again, stop if the remote has changed, and ask again if anything else would go. Only the command line's `--yes` goes ahead whatever it comes to.
- Put the source and destination last, after `--`. Given a flag after them, the `rsync` on macOS takes the current folder as the destination and empties it.

The Site menu does the same in the app, in the background, on a notebook only. With no terminal for ssh to ask, it runs with `BatchMode=yes` and a 15 second connect timeout, so a key that isn't in an agent or a new host fails with a message. A job with nothing to report says so in the status row, and warnings, errors and a question about deletions open a window.

## 8. Syncing

A notebook doesn't need syncing, and syncing doesn't need a notebook. `.kame-logs` turns on revision logs for a folder and everything under it ([merging](merging.md)), and it's often put in a notebook's root.

kame only syncs the open document. A closed one can be behind what another machine wrote until it's next opened, and search, the links list and `kame-site` all read what this machine has.

A document with only logs is read from them, and its database is built when it's opened.

To delete a document in a logged folder, remove both `X.kame` and `.X.kame.sync/`. Removing only the database gets it rebuilt from its logs.

Moving a document by hand keeps its new place, and links to the old path don't follow. Following a link to the old path then creates a new, empty document there and opens it, and it's removed if left untouched.

## 9. Known weak points

These describe the code as it stands, and aren't rules to copy.

- **Some untouched documents aren't removed.** Only one newly created document is remembered at a time, so following a link from one untouched new document to create another keeps the first, and it may then write logs. Cancelling the prompt about losing work also keeps it.
- **Links can publish what they shouldn't, or nothing useful.** A link to an existing private folder publishes its name (`/papers/`) with no warning. A link to `./` in a root with no index publishes `href="//"`.
- **A move can't change links in Markdown.** kame imports `.md` and `.markdown` files rather than editing them, so links in them are left as they are, and the report after a move names each one.
- **Reading every document is the cost of storing nothing.** Lists with titles, search and the links list read each file, so the app does them in the background. While the browser or jump list shows, a periodic check re-reads only if the entries changed. So a title changed on another machine shows when one of those is next opened, another document becomes current, or the entries change.
- **Only Syncthing's conflict copies are recognised.** See [merging, section 7](merging.md).
