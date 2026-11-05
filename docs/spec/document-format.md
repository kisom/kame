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

A reader should also refuse a `user_version` above 1, and treat a missing `ops`, `metadata` or `snapshots` table as empty, since read-only opens don't add them. kame's read-only path does neither: it reads a newer file without complaint, and an older one lacking a table gives an error when that table is read.

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
- **Ops unreadable:** the text is what matters, so open anyway and warn. In one transaction, keep the head, set the cursor to 0 and empty the log, since a later edit would otherwise fall out of step with it. So one row that kame can't parse, from any program, costs the whole history.
- **No blocks in the head:** refuse to open.
- **Metadata missing or unreadable:** use defaults, and try to save them: the title, unless it's `Untitled`, and the file's creation date as `created`. Unreadable metadata is therefore replaced.

### 1.6 While a file is open

Another program can rename a new file over an open one. kame compares the device and inode at the path with those it opened before each edit, undo and redo. If they differ, it stops writing to that path and keeps the text in memory until the writer saves it elsewhere. Metadata, position, sync state, snapshots and compacting aren't checked, so those go to the old file and are lost, though saving elsewhere writes the metadata again. It removes the `-wal` and `-shm` beside the path only if the `-wal` is still the one it had open. SQLite would otherwise pair its old log with the new file.

### 1.7 Saving under a new name

Saving as another file must not expose a partial or wider-readable copy. kame creates a hidden `.NAME.saving-UUID` beside the target with mode `0600`, runs `VACUUM INTO` to it, sets its final mode, removes any `-wal` and `-shm` at the target and renames it into place. The final mode is the stricter of the target's and the current file's. It's `0600` if either can't be read, and the system default only when neither exists.

Afterwards, kame works out the path of each image whose file can be found for the new location, relative inside a notebook and whole otherwise, and writes any change as an ordinary `SetBlockKind` edit.

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

### 2.2 Blocks

Each block has an `id` (UUID), `kind`, `text` and `spans`, all required.

A kind with no fields is written as a string. A kind with fields is written as an object with one key:

| JSON | Meaning | Text |
|---|---|---|
| `"Paragraph"` | Body text | Prose |
| `{"Heading":{"level":2}}` | A heading. Typing makes levels 1 to 3, and importing Markdown 1 to 6 | Prose |
| `{"ListItem":{"ordered":true,"depth":1}}` | A list item. `depth` is left out when 0 | Prose |
| `"BlockQuote"` | A quotation | Prose |
| `{"Image":{"path":"images/plate.jpg","link":"https://…","align":"Center"}}` | An image. `link` is left out when absent. `align` is `Left`, `Center` or `Right`, and is left out when `Left` | Empty (see section 8) |
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
| `"Note"` | A private note on a passage, starting with an empty body | Byte range | The body never. HTML marks the passage with `<mark>`, and Markdown shows it as plain text |
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

A mismatch is a bug: **stop, and persist nothing**. Never drop a mismatched payload, because that would lose notes. An implementation should validate before changing anything. kame panics, so its disk stays as it was, but its memory doesn't, because `DeleteText` checks `removed` after changing the text.

`apply` also fails on an unknown `block_id`, an out-of-range index or a position that's not a character boundary. `AddAnnotation` checks nothing, so an implementation must validate the anchor first, as kame's command does. Nothing checks at open that the log fits the head, so drift only shows at undo.

| Operation | Fields |
|---|---|
| `InsertText` | `block_id`, `offset`, `text` |
| `DeleteText` | `block_id`, `start`, `end`, `text`, `removed`, `old_spans` |
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

All fields are required except on `DeleteText`, where `removed` defaults to empty and `old_spans` may be absent. They're absent in rows from before they existed, where undo can't restore those notes or spans, and inside a `Batch` that restores spans itself. A row missing a required field makes the whole log unreadable (section 1.5). kame's own commands no longer emit `SetTitle`, but readers must still apply it.

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

Spans follow the same steps, except that dropped spans aren't checked against any payload, and a span that ends up empty is dropped. `old_spans` isn't checked either.

**`InsertBlock`.** `index` must be at most the number of blocks.

**`DeleteBlock`.** The block at `index` must have the id of `block`. `annotations` must equal the annotations anchored in that block, compared as whole values and in any order. It removes the block and all of them, including whole-block anchors.

**`SetBlockKind`.** `old_kind` must match the block's kind.

**`SetSpans`.** Replaces the block's spans. `old_spans` isn't checked, because spans are only formatting and older histories can drift from them.

**`AddAnnotation`.** Appends. **`RemoveAnnotation`.** Removes by `id`.

**`SetAnnotationBody`.** The annotation must exist and `old_body` must match.

**`SetTitle`.** `old` must match. **`ReplaceDocument`.** Replaces the whole document, without checking `old`.

**`Batch`.** Members apply in order, each to the result of the last, on a copy that replaces the document only when all have applied. Each member's payload must be built from the document as it'll be when that member applies. Undoing a `Batch` isn't staged like this in kame, and an implementation should stage it.

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

- After undoing `DeleteText`, set the block's spans to `old_spans` if present, then append each annotation in `removed`.
- After undoing `DeleteBlock`, append each annotation in `annotations`.
- Undo a `Batch` member by member in reverse, each by these rules. Don't invert it whole first, which would lose the payloads.

Redo applies the original operation.

**A note can change shape through delete and undo,** because its anchor keeps no record of its old extent. For a note `a..b` and a deletion `s..e`, after undo:

| The note | Becomes |
|---|---|
| Strictly contains the deletion | `a..b`, restored exactly |
| Starts before it and ends inside it or at its end | `a..s` |
| Starts at or inside it and ends after it | `e..b` |

Snapshots are the way back from that. **So an undo may not apply.** A note that has changed shape can come to lie wholly inside an earlier insert, and undoing that insert then fails the `removed` check. kame panics in that case (section 8). An implementation should undo on a copy and write nothing on failure.

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
- A single deleted character (not a line feed) that continues the last delete in the same block, backward or forward, unless either delete removed annotations or it crosses into a new word.
- Successive changes to the same annotation's body, unless the current body ends in whitespace and the next doesn't continue it with more whitespace. Any other change joins, whatever its size.
- Successive `ReplaceDocument` arrivals from another machine with no edit between them.

Only a single character can join onto an insert or delete, but the earlier op can be any length. The joined `InsertText` keeps the first `offset`. The joined `DeleteText` extends `start`, `end` and `text`, keeps the first `old_spans`, and has `removed` empty. The joined `SetAnnotationBody` keeps the first `old_body`.

**Compacting** applies the first three joins to an existing log, leaving the redo tail alone. kame first replays the undo part of the old and new logs and changes nothing unless they agree. It compares only block ids, kinds and text, and the set of note ids, so spans, anchors, note bodies and the title aren't compared.

The head is stored whole, so nothing needs replaying to open a file. The log is only for undo.

## 6. Snapshots

A snapshot is a complete document in the `json` column, with `timestamp` in Unix seconds and an optional `label`. `timestamp` and `json` must not be NULL, though the schema allows it: a NULL `timestamp` makes kame's list of snapshots fail. It's independent of the head and log, and undo and redo can't touch it. Lists show the newest first, by `timestamp` and then `id`.

Restoring isn't an edit. It starts a new history (section 5) and leaves metadata alone. A snapshot with no blocks can't be restored.

## 7. Compatibility

Version 1 has grown by adding things that older readers can skip:

- `ops`, `snapshots` and `metadata` tables, added on open.
- `depth`, `link`, `align`, `language`, `removed` and `old_spans`, all optional on reading and mostly left out when empty.
- Range anchors on marginal notes, from before whole-block anchors.

A change that older readers could misread must raise `user_version`. Older builds then refuse the file with a clear message when opening it to write. kame's read-only path doesn't check, so its site build, notebook listing and sync tools can misread such a file (section 1.2).

A new optional field must be left out when it holds its default. Revision logs hash these bytes, and [merging, section 9](merging.md) sets out what happens otherwise.

## 8. Known weak points

These describe the code as it stands, and aren't rules to copy.

- **Unknown fields are lost.** Readers ignore fields they don't know, and rewriting the head drops them. An older build can silently remove a newer field, so a new field must be safe to lose or else raise `user_version`. The optional `align` on `Image` behaves this way: an older build rewriting the document puts the image back on the left. In revision logs the result is worse: see [merging, section 9](merging.md).
- **Unknown kinds and styles aren't skipped.** One unknown block kind, style or annotation kind makes the head unreadable, so the document won't open. One in the log makes the history unreadable, and opening then discards it (section 1.5). A new variant must raise `user_version`.
- **Ranges aren't checked on reading.** Nothing checks a span or anchor against its text when a document is read, so a damaged or hand-made file can hold offsets out of range or inside a character. Readers should clamp them for display, and must not assume they're valid.
- **The rules in section 2 aren't enforced on reading.** `level` and `depth` from 0 to 255 aren't checked, and anything outside that makes the head unreadable. Text on `Image` and `Rule` blocks isn't checked either. Keep to the rules when writing and tolerate them when reading.
- **Typing on an image block stores text in it.** `Session::insert` starts a new paragraph on a `Rule`, but has no such case for an `Image`. This is a bug to fix.
- **An undo can panic.** The inverse of `InsertText` assumes no note can lie wholly inside the inserted text, but section 4.2 shows how one can come to. For example, with text `0123456789` and a note on 4..6: insert `abc` at 5, delete 8..10 and undo it, delete 3..6 and undo it, then undo the insert. This is a bug to fix. Undoing a `Batch` that fails partway also leaves the document half undone in memory.
- **Paths aren't portable by themselves.** A whole image path or a `~/` link only works where it was made. Only relative forms hold on another machine.
