# The kame document format

Version 1, as written by kame at commit `aa493c7`. This is what a `.kame` file holds and what a program must do to read and change one safely. Where this document and `kame-core` disagree, that's a bug in one of them, and until it's settled the code is what's in everyone's files.

"Must" marks what another implementation needs in order to share files with kame. Notebooks are in [notebook.md](notebook.md), and merging and revision logs are in [merging.md](merging.md).

## 1. The file

A `.kame` file is a SQLite 3 database holding one document. It's a local working file. Copying it while it's being written can damage it, so moving a document's changes between machines belongs to [merging](merging.md), not to file sync.

### 1.1 Opening

**Only one program may write a file at a time.** kame holds no lock between transactions, and it notices a file being replaced but not another connection committing to it. With two writers on one file, the second's head overwrites the first's text while the log keeps both, and a later undo fails. A writer must either be the only one, or read the head and cursor again before each write.

A writer must, in this order:

1. Open the database, waiting up to 5 seconds on a lock.
2. Set `PRAGMA journal_mode = WAL`, and refuse the file if the mode that comes back isn't `wal`. An in-memory database reports `memory`, which is also accepted.
3. Read `PRAGMA user_version`, and refuse the file if it's above 1. It must not be written back down.
4. Run the schema below, then set `user_version` to 1 if it was 0.

While the file is open, SQLite keeps `-wal` and `-shm` files beside it. A clean close checkpoints with `PRAGMA wal_checkpoint(TRUNCATE)` and removes both. After a crash they stay, and SQLite recovers from them on the next open.

### 1.2 Reading without writing

A reader that must change nothing, such as a site build, has three cases:

| Beside the file | What it means | How to read |
|---|---|---|
| No `-wal` | Nothing is waiting in a log | Open read-only with `immutable=1`, which creates no sidecars |
| `-wal` and `-shm` | A writer may have it open, with newer text | Open read-only as usual |
| `-wal` alone | Left by a crash or a sync | Copy the file and the `-wal` somewhere private, and read the copy |

A reader must also refuse a `user_version` above 1, and should treat a missing `ops`, `metadata` or `snapshots` table as empty, since read-only opens don't add them. kame gives an error when such a table is read.

### 1.3 Schema

`user_version` is 1. Every table is created with `IF NOT EXISTS`, so an older version 1 file gains any it lacks when opened.

```sql
CREATE TABLE IF NOT EXISTS documents (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ops (
    seq  INTEGER PRIMARY KEY AUTOINCREMENT,
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS metadata (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS snapshots (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp INTEGER,
    label     TEXT,
    json      TEXT
);
```

| Table | Holds |
|---|---|
| `documents` | The head: the current document as JSON, in the one row `id = 1` (section 2) |
| `ops` | The history, one JSON operation per row, in `seq` order (section 4) |
| `meta` | Small values by key (section 1.4) |
| `metadata` | What the document says about itself, as JSON, in the one row `id = 1` (section 3) |
| `snapshots` | Complete copies of the document, in the same JSON as the head (section 6) |

### 1.4 `meta` keys

| Key | Value | Meaning |
|---|---|---|
| `undo_cursor` | Decimal integer, as text | How many ops, in `seq` order, are applied to the head. Absent means 0 |
| `position` | JSON: `{"block_id":"…","offset":0,"scroll":0.0}` | Where the writer last was. A convenience, not an edit |
| `sync` | Text | Belongs to the sync tool, and is opaque here. See [merging](merging.md) |

A writer must keep `meta` keys it doesn't know, and all snapshots, whatever else it rewrites.

An `undo_cursor` that isn't a non-negative integer is corruption, and the file must not open as if it were 0, since that would bring undone ops back. A cursor past the end of the log is treated as the end. An unreadable `position`, or one naming a block that's gone, counts as none. An `offset` is clamped to the text and moved back to a character boundary.

### 1.5 Opening a document

- **No head row:** a new document. Write `Document::new` with the file's stem as its title.
- **Head unreadable:** refuse to open. The file must not be overwritten.
- **Anything from a newer version:** open read-only and say so (section 7). That means a field in the head or metadata that would be lost by saving, or an op of a kind this build doesn't know. Nothing may be written over the document, including by retrying, Save As, restoring a snapshot or changing metadata.
- **Ops otherwise unreadable:** the text is what matters, so open anyway and warn. In one transaction, keep the head, set the cursor to 0 and empty the log, since a later edit would otherwise fall out of step with it. So one damaged row, from any program, costs the whole history.
- **No blocks in the head:** refuse to open.
- **Metadata missing or unreadable:** use defaults, and try to save them: the title, unless it's `Untitled`, and the file's creation date as `created`. Unreadable metadata is therefore replaced.

### 1.6 While a file is open

Another program can rename a new file over an open one. kame compares the device and inode at the path with those it opened before each edit, undo and redo. If they differ, it stops writing to that path and keeps the text in memory until the writer saves it elsewhere. Metadata, position, sync state, snapshots and compacting aren't checked, so those go to the old file and are lost, though saving elsewhere writes the metadata again. It removes the `-wal` and `-shm` beside the path only if the `-wal` is still the one it had open. SQLite would otherwise pair its old log with the new file.

Only one kame has a document open. It holds an advisory `flock` on a file of its own, `open/DEVICE-INODE.lock` in kame's settings folder, named for the document's device and inode, so that every spelling of its path finds the same lock. The lock is never on the document or beside it: SQLite holds POSIX locks on the database, which `flock` can conflict with on macOS, and what's beside a document travels with a synced notebook. It's taken before anything is written to the document, by opening it, saving over it or merging into it, and another kame holding it refuses all three. Leaving the document, or quitting once it's closed, lets it go, and removes the lock file while it's still held, if the file at that path is still the one it locked. A taker that finds the file it locked is no longer at that path, because its holder removed it meanwhile, lets it go and tries again, so no two hold one document. A file that isn't there yet has nothing to lock until it's made: an open that makes it takes the lock as soon as it's there, and leaves the file, as a failed open does, if another kame making it at the same moment has it. A Save As takes the lock on its hidden copy before renaming it into place. Anything else that finds another kame has its file stops writing to it, as for a replaced file (section 1.6), and the words wait for a Save As to another name. A lock that can't be taken, as when the settings folder can't be written, counts as nobody else's.

### 1.7 Saving under a new name

Saving as another file must not expose a partial or wider-readable copy, or one that says its paths from the old place. kame creates a hidden `.NAME.saving-UUID` beside the target with mode `0600`, runs `VACUUM INTO` to it and opens it. In the copy it forgets the sync state, then writes the head, the whole log, the cursor and the metadata as they are in memory, which after a failed write is more than the old file's log holds, with each relative link and image path said again for the new location, in them and in every snapshot, in one transaction that isn't an edit. The old file isn't written first, apart from its metadata, at best, so one that can't be written can still be saved elsewhere. It then puts all of the copy's `-wal` into the file, failing if it can't, sets its final mode, removes any `-wal` and `-shm` at the target and renames it into place. The final mode is the stricter of the target's and the current file's. It's `0600` if either can't be read, and the system default only when neither exists.

An image whose file can be found is said relative inside a notebook and whole otherwise. A link to the document itself follows its new name. If the snapshots can't be read, they're kept as they were and kame says so, since the words matter more. If the transaction fails, the Save As fails, as a failed copy does.

Saving as the document's own file writes nothing, unless a write has failed. Its own file is the target spelled as the open path, or the file kame has open, by device and inode, however the target names it: through a symlink, in another case on a disk that ignores case, or by a hard link under another name. Once another file has been put at the open path (section 1.6), saving onto that file, by either spelling, is refused, and the file kame opened, wherever it is now, is saved over as any other file is. Then it's a retry, as the error strip's is: the head, the whole log, the cursor and the metadata from memory, in one transaction.

## 2. The document

All JSON is UTF-8 and stored compact, without the whitespace in the examples here. Write fields in the order shown to match kame. Revision logs recompute digests from parsed values, so the order in a `.kame` file doesn't affect them, but a program computing digests itself must match it exactly ([merging, section 2](merging.md)). Readers must accept any order and ignore fields they don't know.

UUIDs are written lowercase and hyphenated, and new ones are version 4. Offsets and indexes are non-negative integers.

```json
{
  "id": "00000000-0000-0000-0000-000000000001",
  "title": "Sample",
  "blocks": [
    {
      "id": "00000000-0000-0000-0000-000000000011",
      "kind": "Paragraph",
      "text": "A bold link",
      "spans": [
        { "start": 2, "end": 6, "style": "Bold" },
        { "start": 7, "end": 11, "style": { "Link": { "url": "https://example.com" } } }
      ]
    }
  ],
  "annotations": [
    {
      "id": "00000000-0000-0000-0000-000000000020",
      "kind": "Marginal",
      "anchor": "00000000-0000-0000-0000-000000000011",
      "body": "aside"
    },
    {
      "id": "00000000-0000-0000-0000-000000000022",
      "kind": "Footnote",
      "anchor": {
        "block_id": "00000000-0000-0000-0000-000000000011",
        "start": 11,
        "end": 11
      },
      "body": "foot"
    }
  ]
}
```

| Field | Type | Notes |
|---|---|---|
| `id` | UUID | Identifies the document across copies. Merging relies on it |
| `title` | string | Set from the file name at creation. A fallback when `metadata.title` is empty (section 3) |
| `blocks` | array | Reading order. Must hold at least one block after every operation, since kame refuses a file with none. A command that removes the last block must add one in the same `Batch` |
| `annotations` | array | Storage order means nothing (section 2.5) |

All four fields are required. A new document has one empty `Paragraph`. Block ids must be unique within a document, and so must annotation ids. Nothing enforces this. Lookups take the first match, and `RemoveAnnotation` removes all matches.

### 2.1 Offsets

**Every offset is a count of UTF-8 bytes into a block's `text`, and must fall on a character boundary.** That includes span and anchor ranges, cursor positions and op offsets. "é" is 2 bytes and "🐢" is 4. Kotlin, Java and JavaScript strings count UTF-16 units, so offsets must be converted wherever text crosses into them.

A damaged or hand-made file can still hold a span or range anchor past its text, inside a character or reversed, so kame fits them whenever it reads a document: the head, a snapshot, a document or block in an op, and a revision from a log. It fits the notes a `DeleteBlock` or `DeleteText` carries too, against the block or the text the op carries, and a merge fits what it's given. Each offset is brought into the block's text and back to a character boundary. A span left empty or reversed is dropped, and a reversed anchor becomes a point at its start. One that already fits, and an anchor whose block is gone, are left alone. This is only in memory, so the file keeps what it had until the head is next written, and a revision's id still checks.

### 2.2 Blocks

Each block has an `id` (UUID), `kind`, `text` and `spans`, all required.

A kind with no fields is written as a string. A kind with fields is written as an object with one key:

| JSON | Meaning | Text |
|---|---|---|
| `"Paragraph"` | Body text | Prose |
| `{"Heading":{"level":2}}` | A heading. Typing makes levels 1 to 3, and importing Markdown 1 to 6 | Prose |
| `{"ListItem":{"ordered":true,"depth":1}}` | A list item. `depth` is left out when 0 | Prose |
| `"BlockQuote"` | A quotation | Prose |
| `{"Image":{"path":"images/plate.jpg","link":"https://…","align":"Center"}}` | An image. `link` is left out when absent. `align` is `Left`, `Center` or `Right`, and is left out when `Left` | Empty |
| `"Rule"` | A horizontal rule | Empty |
| `{"Code":{"language":"rust"}}`, `{"Code":{}}` | Code. `language` is left out when absent | Verbatim |
| `"Prose"` | Text quoted exactly, in the serif | Verbatim |

Verbatim text keeps its line feeds and spacing exactly, and a line break within it is a `\n`. Prose text normally holds no line feeds, because a pasted line break makes a new block, but importing a Markdown hard break can put a `\n` in one, so readers must accept it.

An unknown kind fails the whole document, because Rust's reader rejects it. See section 8.

### 2.3 Spans

A span has `start`, `end` and `style`, with `start < end`. It's the half-open byte range `start..end`.

| `style` | Meaning |
|---|---|
| `"Bold"`, `"Italic"` | Emphasis |
| `"Code"` | Inline code |
| `{"Link":{"url":"…"}}` | A hyperlink |
| `{"DocumentLink":{"target":"…"}}` | A link to a kame document or a heading, kept exactly as entered |

Spans may overlap and may cross, so bold 0..10 and italic 5..15 is allowed. A link entered as a URL becomes a `DocumentLink` if the part before any `#` ends in `.kame` in any case and has no `://`, or if it's just a `#` and a heading name. Otherwise it's a `Link`.

Bare URLs in text aren't stored as spans. They're recognised when read: `http://`, `https://` or `www.`, up to the next whitespace, without trailing `.,;:!?)]}`. They're ignored in code blocks and inline code.

Script links are refused at the edge: a URL whose scheme reads `javascript:`, once control characters and spaces are removed and case is ignored, must not be followed. Don't rely on a file being free of them.

### 2.4 Paths and targets

An image's `path` is one of three things. A whole path or a URL with `://` is used as it stands. Anything else is relative to the folder holding the document. Inside a notebook kame stores the relative form, with `/` separators and `..` where needed.

A document link's `target` is `PATH#HEADING`, with either part optional. `~/` is the home directory, and a relative path is relative to the folder holding the document. The path is tried as entered, then with `.kame`, then with `.md`. Only a file that exists counts. `HEADING` names the first heading whose whole text matches exactly.

### 2.5 Annotations

An annotation has `id` (UUID), `kind`, `anchor` and `body`, all required. `body` is plain text.

| `kind` | What it is | Anchor | Published |
|---|---|---|---|
| `"Marginal"` | A note beside a block | Whole block | In HTML only |
| `"Note"` | A private note on a passage, starting with an empty body. With no words, it's purely a highlight | Byte range | The body never. A note with words leaves no trace. One with an empty or blank body is marked with `<mark>` in HTML, and shown as plain text in Markdown |
| `"Footnote"` | A numbered note | Byte range, normally the words it was added to, and zero-width when imported. The marker sits at `end` | Yes |

An anchor takes one of two forms, told apart by JSON type:

- **Whole block:** a bare string, the block's id.
- **Range:** `{"block_id":"…","start":2,"end":6}`, where `start <= end`.

Older files anchor some marginal notes as ranges, which readers must accept.

An anchor can name a block that no longer exists. HTML keeps the body of a footnote or marginal note whose block is missing or empty, without a marker and numbered last, sorted by id. Markdown drops it.

Numbering never depends on storage order: sort by block position, then anchor `start`, then `end`, then annotation `id`. Because restoring a note appends it, two sets of annotations must be compared as sets.

### 2.6 Lists

List numbers aren't stored. Reading blocks in order, with a stack of counts by level:

1. Anything but a list item clears the stack and ends the list.
2. An item's level is its `depth`, capped at 5, then capped at the current stack size. So it's never more than one level below the item before.
3. Cut the stack to `level + 1` entries, adding a 0 if needed.
4. A numbered item adds one to its level's count. A bullet resets it to 0 and has no number.

Exports skip empty blocks, other than rules and images, and those neither end a list nor take a number. The page doesn't skip them.

On the page, a number shows its parents' numbers: `1.`, then `1.1`, then `1.1.1`. A bullet has no label and adds nothing to the numbers under it.

## 3. Metadata

```json
{"title":"Sample","author":"A","created":"2026-11-04","published":"","tags":["t"],"words":["kame"],"accepted":[{"rule":"R","text":"x"}],"publish":true}
```

| Field | Type | Meaning |
|---|---|---|
| `title` | string | The title people see. If empty, kame falls back to `document.title` and then the file's stem, though the window title goes straight to the stem. Most paths skip `Untitled`, and the notebook list doesn't |
| `author` | string | |
| `created`, `published` | string | Free text, written `YYYY-MM-DD` by kame in UTC |
| `tags` | array of strings | |
| `words` | array of strings | Words the spell checker takes as right in this document |
| `accepted` | array of `{rule, text}` | Marks the writer has said the checker is wrong about |
| `publish` | boolean | Whether a notebook's site publishes this document |

Every field is optional on reading, and defaults to empty or false. Writers write them all.

Metadata isn't part of history. Changing it writes the `metadata` row directly, with no op, and restoring a snapshot leaves it alone.

## 4. Operations

Each row in `ops` is one JSON object with a single key naming the operation. **All text changes must go through an operation**, so that history can always put the document back.

Removals carry what they remove, so that undoing one needs nothing else. `apply` checks `DeleteText.text`, `DeleteText.removed`, `DeleteBlock.annotations` and the id of `DeleteBlock.block`. `RemoveAnnotation` isn't checked, a missing id does nothing, and undo restores what the op carries, so a wrong payload there changes the document silently.

A mismatch is a bug: **stop, and persist nothing**. Never drop a mismatched payload, because that would lose notes. An implementation should validate before changing anything. For a new edit, kame panics, so its disk stays as it was, but its memory doesn't, because `DeleteText` checks `removed` after changing the text. Undo and redo run on a copy and refuse the step instead.

`apply` also fails on an unknown `block_id`, an out-of-range index or a position that's not a character boundary. `AddAnnotation` checks nothing, so an implementation must validate the anchor first, as kame's command does. Nothing checks at open that the log fits the head, so drift only shows at undo or redo.

| Operation | Fields |
|---|---|
| `InsertText` | `block_id`, `offset`, `text` |
| `DeleteText` | `block_id`, `start`, `end`, `text`, `removed`, `old_spans`, `clipped` |
| `InsertBlock` | `index`, `block` |
| `DeleteBlock` | `index`, `block`, `annotations` |
| `SetBlockKind` | `block_id`, `old_kind`, `kind` |
| `SetSpans` | `block_id`, `old_spans`, `spans` |
| `AddAnnotation` | `annotation` |
| `RemoveAnnotation` | `annotation` |
| `SetAnnotationBody` | `annotation_id`, `old_body`, `new_body` |
| `SetTitle` | `old`, `new` |
| `ReplaceDocument` | `old`, `new`, each a whole document |
| `Batch` | `ops`, an array of operations |

```json
{"InsertText":{"block_id":"00000000-0000-0000-0000-000000000011","offset":0,"text":"x"}}
{"DeleteText":{"block_id":"00000000-0000-0000-0000-000000000011","start":0,"end":2,"text":"A ","removed":[],"old_spans":[{"start":2,"end":6,"style":"Bold"}]}}
{"SetBlockKind":{"block_id":"00000000-0000-0000-0000-000000000011","old_kind":"Paragraph","kind":"BlockQuote"}}
{"Batch":{"ops":[{"SetTitle":{"old":"a","new":"b"}}]}}
```

All fields are required except on `DeleteText`, where `removed` and `clipped` default to empty and `old_spans` may be absent. `clipped` is left out when empty, and holds the annotations this delete cut short, as they were before it, which is what lets undo restore them. These fields are absent in rows from before they existed, where undo can't restore those notes or spans. `old_spans` is also absent inside a `Batch` that restores spans itself. A row missing a required field makes the whole log unreadable, and costs the history (section 1.5). kame's own commands no longer emit `SetTitle`, but readers must still apply it.

### 4.1 Applying

**`InsertText`.** The offset must be a character boundary within the text. Let `n` be the inserted length in bytes. For each range anchor and span in the block:

1. If `start < offset < end`, add `n` to `end`, so it widens.
2. If `start >= offset`, add `n` to both, so it moves right.

So text inserted exactly at an anchor's start pushes it along, and text inserted exactly at its end stays outside it. Anchors in other blocks and whole-block anchors don't change.

**`DeleteText`.** `start..end` must be a character-bounded range and `text` must match it exactly. If `start == end`, annotations and spans don't change, and `removed` must be empty. Otherwise, for each range anchor in the block:

1. Drop it if it lies wholly inside the deletion. That means `start <= a` and `b <= end` for a non-empty anchor `a..b`, and `start <= p < end` for a zero-width anchor at `p`. So deleting the space before a footnote moves the footnote and doesn't drop it.
2. The dropped set must equal `removed`, compared as whole values and in any order.
3. If it overlaps the deletion (`a < end` and `b > start`), remap each end: an offset at or before `start` stays, one at or after `end` moves back by the deleted length, and one inside becomes `start`.
4. If it lies wholly after, so its original `a >= end`, move both ends back by the deleted length.

Spans follow the same steps, except that dropped spans aren't checked against any payload, and a span that ends up empty is dropped. `old_spans` and `clipped` aren't checked either.

A writer must set `clipped` to every range anchor that step 3 changes, in terms of the document as it is when this delete applies. For a member of a `Batch`, that's after the members before it. kame fills it in once, for every edit, rather than leaving it to each command.

**`InsertBlock`.** `index` must be at most the number of blocks.

**`DeleteBlock`.** The block at `index` must have the id of `block`. `annotations` must equal the annotations anchored in that block, compared as whole values and in any order. It removes the block and all of them, including whole-block anchors.

**`SetBlockKind`.** `old_kind` must match the block's kind.

**`SetSpans`.** Replaces the block's spans. `old_spans` isn't checked, because spans are only formatting and older histories can drift from them.

**`AddAnnotation`.** Appends. **`RemoveAnnotation`.** Removes by `id`.

**`SetAnnotationBody`.** The annotation must exist and `old_body` must match.

**`SetTitle`.** `old` must match. **`ReplaceDocument`.** Replaces the whole document, without checking `old`.

**`Batch`.** Members apply in order, each to the result of the last, on a copy that replaces the document only when all have applied. Each member's payload must be built from the document as it'll be when that member applies. Undo and redo stage the whole step the same way.

### 4.2 Undoing

The inverses are:

| Operation | Inverse |
|---|---|
| `InsertText` | `DeleteText` of that range, with `removed` empty and `old_spans` absent |
| `DeleteText` | `InsertText` at `start` |
| `InsertBlock` | `DeleteBlock`, with `annotations` empty |
| `DeleteBlock` | `InsertBlock` |
| `SetBlockKind`, `SetSpans`, `SetAnnotationBody`, `SetTitle`, `ReplaceDocument` | The same operation with old and new swapped |
| `AddAnnotation` | `RemoveAnnotation`, and the reverse |
| `Batch` | Each member inverted, in reverse order |

Inverting isn't quite enough, so undo must also restore what the operation carried:

- After undoing `DeleteText`, set the block's spans to `old_spans` if present, append each annotation in `removed`, then give each annotation in `clipped` its old anchor, by id.
- After undoing `DeleteBlock`, append each annotation in `annotations`.
- Undo a `Batch` member by member in reverse, each by these rules. Don't invert it whole first, which would lose the payloads.

Redo applies the original operation.

**Undo and redo are exact,** for history with `clipped`, so anything they check still matches. In rows without it, a note cut short can stay changed after undo, because its anchor keeps no record of its old extent. For a note `a..b` and a deletion `s..e`, undo then gives:

| The note | Becomes |
|---|---|
| Strictly contains the deletion | `a..b`, restored exactly |
| Starts before it and ends inside it or at its end | `a..s` |
| Starts at or inside it and ends after it | `e..b` |

**So an undo of older history may not apply.** A note that has changed shape can come to lie wholly inside an earlier insert, and undoing that insert then fails the `removed` check, and redoing a delete can fail it the same way. An implementation must undo and redo on a copy and write nothing on failure. kame refuses that step and says so, and the history stays where it is, so the document can still be edited.

### 4.3 Commands

Commands choose which operations to write, and the format doesn't fix those choices. Most commands that need several changes, such as splitting a block, write one `Batch`, so one undo reverses them. A multi-line paste, typing over a selection and rewriting image paths each write several entries. A split moves the tail by deleting it from one block and adding it to a new one, and it moves the anchors in the tail with it.

## 5. History on disk

History is linear. The first `undo_cursor` ops in `seq` order are applied to the head, and any after that are the redo tail. The cursor is a count, not a `seq`, and `seq` values can have gaps.

Each of these is one transaction, because head, log and cursor must never disagree:

| Action | Writes |
|---|---|
| Edit | Delete the redo tail, append the op, replace the head, add one to the cursor |
| Edit joining the last | Replace the last op, and replace the head |
| Undo or redo | Replace the head and set the cursor. The log doesn't change |
| Restore a snapshot | Replace the head, delete every op, set the cursor to 0 |
| Compact | Replace the log and set the cursor, then `VACUUM` and checkpoint |

**Joining** is optional for a writer and means nothing to a reader, but without it history grows by about 1 KB per word. kame joins an edit onto the last one only when there's no redo tail. It joins:

- A single inserted character (not a line feed) that continues the last insert, unless that insert ended in a line feed or this character starts a new word after whitespace.
- A single deleted character (not a line feed) that continues the last delete in the same block, backward or forward, unless either delete removed annotations, this one cut a note short, or it crosses into a new word.
- Successive changes to the same annotation's body, unless the current body ends in whitespace and the next doesn't continue it with more whitespace. Any other change joins, whatever its size.
- Successive `ReplaceDocument` arrivals from another machine with no edit between them.

Only a single character can join onto an insert or delete, but the earlier op can be any length. The joined `InsertText` keeps the first `offset`. The joined `DeleteText` extends `start`, `end` and `text`, keeps the first `old_spans` and `clipped`, and has `removed` empty. The joined `SetAnnotationBody` keeps the first `old_body`.

**Compacting** applies the first three joins to an existing log, leaving the redo tail alone. kame first replays the undo part of the old and new logs and changes nothing unless they agree. It compares only block ids, kinds and text, and the set of note ids, so spans, anchors, note bodies and the title aren't compared.

The head is stored whole, so nothing needs replaying to open a file. The log is only for undo.

## 6. Snapshots

A snapshot is a complete document in the `json` column, with `timestamp` in Unix seconds and an optional `label`. `timestamp` and `json` must not be NULL, though the schema allows it: a NULL `timestamp` makes kame's list of snapshots fail. It's independent of the head and log, and undo and redo can't touch it. A Save As, to any folder, says a snapshot's relative links and image paths again, in place, keeping its `id`, `label` and `timestamp` (section 1.7), and leaves one this build can't read whole as it is. Lists show the newest first, by `timestamp` and then `id`.

Restoring isn't an edit. It starts a new history (section 5) and leaves metadata alone. A snapshot with no blocks can't be restored.

## 7. Compatibility

Version 1 has grown by adding things that older readers can skip:

- `ops`, `snapshots` and `metadata` tables, added on open.
- `depth`, `link`, `align`, `language`, `removed`, `old_spans` and `clipped`, all optional on reading and mostly left out when empty.
- Range anchors on marginal notes, from before whole-block anchors.

A change that older readers could misread must raise `user_version`. Older builds then refuse the file with a clear message.

An older build opening a file with a new field goes read-only until it's updated (section 1.5), which is how a new optional field stays safe. To tell, compare the JSON as written with the JSON after reading and writing it again. A field missing from what was written is only one added since, and doesn't count.

A new optional field must be left out when it holds its default. Revision logs hash these bytes, and [merging, section 9](merging.md) sets out what happens otherwise.

## 8. Known weak points

These describe the code as it stands, and aren't rules to copy.

- **Read-only tools don't check for newer fields.** A site build or a notebook's lists read such a document and leave out what they don't know, so a site built on an older machine can lack a newer feature.
- **Unknown kinds and styles aren't skipped.** One unknown block kind, style or annotation kind makes the head unreadable, so the document won't open, with a message saying it may be from a different version. A new variant in the head should raise `user_version`.
- **Undo can bring back offsets that don't fit.** Some payloads are given against a block's text at a point in history that the op doesn't carry, so they're kept as recorded and not fitted (section 2.1): a `DeleteText`'s `old_spans` and `clipped`, the spans of a `SetSpans`, and the note in an `AddAnnotation` or `RemoveAnnotation`. Undoing or redoing a step recorded against a damaged file puts them back as they were, and they stay so in memory until the document is next read. A merge and a sync fit what they're given, so nothing that maps offsets meets them.
- **Builds that do and don't fit offsets merge a damaged revision differently.** A merge revision is the same whichever machine writes it only if both merge the same documents. Where a revision in the logs holds offsets that don't fit, an older build merges it as it is and this one fitted, so the two write different merges of the same parents, which then merge again, and their checkpoints can differ. It settles once every machine fits offsets. No revision already written changes.
- **The other rules in section 2 aren't enforced on reading.** `level` and `depth` from 0 to 255 aren't checked, and anything outside that makes the head unreadable. Text on `Image` and `Rule` blocks isn't checked either. Keep to the rules when writing and tolerate them when reading.
- **Older files can hold text in an image block,** from before typing on one started a new paragraph. It isn't shown, and nothing removes it.
- **Paths aren't portable by themselves.** A whole image path or a `~/` link only works where it was made. Only relative forms hold on another machine.
- **Save As says what exists on this machine.** A relative `Link` is said again for the new place only if what it names is there, on disk or as revision logs, and nothing limits that to the notebook or the document's folder. A document from elsewhere that comes back after a Save As to another depth shows which of its paths existed here. No contents are read.
- **Not every write takes the lock.** A rename's or move's link rewrites, the title a move writes into the moved document, a reference's rename and recording a database's place write into documents kame doesn't have open, without asking whether another kame has them. A notebook move or delete of a document another kame has open renames or removes the file under it, and that kame finds it changed on disk. Two kames making the same new document at once both write their new document into it before one of them leaves it.
- **A crash during a Save As leaves its copy until the next one.** The hidden `.NAME.saving-UUID`, and its `-wal` and `-shm` once it's open for the paths to be said again, stay beside the target after a crash. The next Save As into that folder removes each copy whose files are all plain files untouched for a day. A Save As onto the document's own file writes no copy and removes nothing. No words are lost, and the UUID keeps a log from pairing with the wrong file.
