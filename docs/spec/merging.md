# Merging kame documents

As built at commit `aa493c7`. This covers how one person's diverged copies of a document become one again: the merge itself, the revision logs that carry changes between machines, and conflict copies. Why it's built this way is in `docs/superpowers/specs/2026-11-03-sync-design.md`. Revision logs are still in trial, opted into by folder.

This builds on the [document format](document-format.md). Sections 2 to 6 are precise because machines have to reach the same bytes separately. **Reuse `kame-sync` rather than reimplementing this.** A second implementation has to match it byte for byte, down to Unicode character classes and JSON escaping, or it will write different revisions from the same inputs.

## 1. What it promises

There is one person and no live collaboration. Merging works on document **states**, not on the op log, so coalescing and compacting a log can't get in its way.

1. **No words of block text are lost.** A deleted block that the other side edited stays, and words that lose a conflict go in a note. Single values that both sides changed, such as a title, a block's kind (which holds an image's path) or most metadata fields, take the later one with no note.
2. **The order of the arguments doesn't matter.** The later version counts as "ours" whichever way round they're passed.
3. **The same inputs give the same bytes.** So two machines merging separately write the same revision and have converged.

## 2. Digests

A digest is the first 16 bytes of SHA-256, as 32 lowercase hex characters.

Readers recompute digests "as read back", which means parsing into the exact schema and serializing again. That drops unknown fields, adds defaults for missing ones, and writes UUIDs canonically. A line that isn't already canonical fails the id check.

What's digested is the compact JSON that `serde_json` writes: no whitespace, with fields in the order the [document format](document-format.md) gives and the order below. Non-ASCII text is written as raw UTF-8. `"` and `\` are escaped, as are characters below U+0020: `\b`, `\t`, `\n`, `\f` and `\r` where they exist, otherwise `\u00xx` in lowercase hex. U+007F and above, and `/`, aren't escaped.

| Digest of | Bytes | Used as |
|---|---|---|
| A block | The block's JSON | Its address in the logs |
| A document | The document's JSON | Telling whether two versions are the same, and breaking ties |
| A revision | The revision's JSON (section 6.2) | The revision's id |

Check an implementation against section 10.

**A derived id** is a UUID whose 16 bytes are the digest of its parts joined by a NUL byte, without setting the version bits. Block and document ids go in as lowercase hyphenated text. Two machines making the same note get the same id.

## 3. Merging text

`merge3(base, ours, theirs)` merges one block's text. All offsets are bytes. Git merges by line, but prose has no lines, and two word-level edits to one sentence can merge cleanly and still make nonsense. So **the sentence is the unit of conflict**: edits on both sides of one sentence conflict, and edits to different sentences never do.

| Base | Ours | Theirs | Result | Conflict |
|---|---|---|---|---|
| `One. Two. Three.` | `One! Two. Three.` | `One. Two. Three?` | `One! Two. Three?` | None |
| `One two.` | `One three.` | `One four.` | `One three.` | Theirs: `One four.` |
| `One two.` | `One three.` | `One three.` | `One three.` | None, since both made the same edit |

### 3.1 Tokens and sentences

**Tokens.** Text splits into runs of word characters, runs of whitespace, and single other characters. A word character is Rust's `char::is_alphanumeric` (`Alphabetic`, plus categories `Nd`, `Nl` and `No`), `'` or `’`. Whitespace is `White_Space`. The Unicode version follows the Rust toolchain, so two builds can disagree on newly assigned characters.

**Sentences.** Walk the tokens with a sentence counter, starting at 0, and a flag saying the last non-space token ended a sentence:

1. The token belongs to the current sentence.
2. If it's all whitespace and either the flag is set or it holds a `\n`, add one to the counter. So the space after a sentence belongs to it, and a line ending always ends one, which makes code merge by line.
3. If it isn't whitespace, the flag becomes true if the token is `.`, `!`, `?` or `…`, or if the flag was already set and the token is `"`, `”`, `’` or `)`. Otherwise it becomes false.

### 3.2 Aligning

Align base tokens with each side's tokens by exact text, finding a longest common subsequence:

1. Match tokens from the front while they're equal, then from the back.
2. If the base's middle times the side's middle is over 4,000,000, or either is empty, stop. The middle counts as rewritten.
3. Otherwise build the usual table from the end. Then walk from the start at row 0, column 0. On equal tokens, match and advance both. Otherwise advance the base if `table[row+1][column] >= table[row][column+1]`, else advance the side.

The tie-break in step 3 decides which alignment wins, so it's part of the format.

### 3.3 Merging against a base

1. **Items.** Base tokens matched on both sides are anchors. Each anchor is an item, and so is each stretch of tokens between anchors, including any at the end. Each item has a range in all three texts.
2. **What changed.** For each item where ours and theirs differ, mark the sentence of the base token at the item's start as changed by ours if ours differs from the base, and by theirs if theirs does. For an item with no base tokens, that's the next base token, so an insertion belongs to the sentence after it, or to the last sentence at the end of the text. A sentence changed by both is **conflicted**.
3. **Building the text.** Go through the items in order. In a conflicted sentence, gather all the items in a row that belong to it into one. Then:

| Case | Text taken | Base | Ours | Theirs |
|---|---|---|---|---|
| Ours equals theirs, and the base | Ours | Kept | Kept | Kept |
| Ours equals theirs, not the base | Ours | Replaced | Kept | Kept |
| Sentence conflicted | Ours, and record a conflict | Lost | Kept | Lost |
| Only ours changed | Ours | Replaced | Kept | Replaced |
| Otherwise | Theirs | Replaced | Replaced | Kept |

The last three columns are each source text's fate in that stretch, which the offset maps use. A conflict records its `range` in the merged text, the base's words and theirs.

### 3.4 Agreed sentences at the ends

Whole sentences that both sides share at the start or end count as agreed, even if the base lacks them. That happens when the base is older than where the two really parted, or there's no base at all. Otherwise every sentence added since would look like a disagreement. Before merging:

1. Find the tokens the two sides share from the front. If one side's tokens run out, all of that side counts as the shared start and nothing as the shared end.
2. Otherwise, the shared start runs up to the start of the sentence, in ours, holding the first difference.
3. Find the tokens they share from the back, without overlapping the start. The shared end is the longest run that begins a sentence on both sides.
4. Leave out either part if the base already starts or ends with it.
5. If anything's left, merge against a new base of `start + base + end`. If the shared start or end already contains the old base, use `start + end`, so that the base isn't counted twice. Find the old base by its first occurrence, trying the start and then the end.
6. Put the base's map back in terms of the old base: keep the stretches overlapping it, clip their source ranges to it and rebase them. Leave their merged ranges as they are, even though a kept stretch clipped at its start then maps early. This affects which spans count as added, so it's part of the format.

### 3.5 Offset maps

Each source text gets a map of `(source range, merged range, fate)` for its non-empty stretches. Anchors and spans go through these:

- **`start(offset)`:** in the stretch where `source.start <= offset < source.end`. If kept, the same position within it. Otherwise the stretch's merged start. If no stretch matches, the end of the merged text.
- **`end(offset)`:** 0 if the offset is 0. Otherwise in the stretch where `source.start < offset <= source.end`. If kept, the same position within it. Otherwise the stretch's merged end. If no stretch matches, the end of the merged text.
- **`span(range)`:** nothing if it shares a byte with a lost stretch, since those words aren't in the text and formatting must not spread across the words that won. Otherwise `start..end`, or nothing if that's empty.

## 4. Merging documents

`merge(base, one, other)` takes an optional base, the latest version both descend from, and two sides, each a document, a `label` for notes and a `when`.

**Shortcuts.** If the two sides have the same digest, return one. If the base has the same digest as one side, return the other. No notes are added.

**Ours and theirs.** Ours is the side with the greater `(when, digest)`. The digest settles a tie, so the argument order can't matter. With no base, use an empty document: the id of ours, an empty title and no blocks or annotations.

### 4.1 Blocks

| Situation | Result |
|---|---|
| Both have it | Merge the text (section 3), take the kind by three-way, and merge the spans |
| One has it, the base doesn't | New, so keep it |
| One has it wholly unchanged from the base, including kind and spans | The other cut it, so the cut stands |
| One has it changed from the base | Keep it and add a note: `Cut on {cutter}, kept because it changed on {keeper}.` |

**Three-way** for a single value: if the base equals ours, take theirs, otherwise ours. So when both changed it, the later wins.

**Conflict notes.** Each text conflict adds a private `Note`. The body is `On {theirs label}: {theirs' words, trimmed}`, or `Cut on {theirs label}.` if theirs is empty once trimmed. It anchors on the conflict's range, or the whole block if the range is empty. Its id is derived from `conflict`, the block id, the base's words, the merged words and theirs untrimmed. A cut note's id is derived from `cut`, the block id and the block's text. The anchor is the whole block.

**Spans.** Move each side's spans through its map (section 3.5), keyed by `(start, end, style's JSON)`, and drop any that map to nothing. Keep a span that both sides have, and one that a side has and the base doesn't. So a span one side removed is gone. Sort by key, comparing the style's JSON as bytes, then join spans of the same style that touch or overlap, since a span moved through an edit can land beside its old self. There's no second sort after joining.

**Order.** Start with ours, keeping only blocks that survive. Then walk theirs: remember the position of each block already placed, and insert each new one just after the last remembered position, or at the start if there's none.

**If no blocks are left,** because each side cut what the other kept, add one empty `Paragraph` with an id derived from `empty` and the document id.

### 4.2 Annotations

Matched by id.

| Situation | Result |
|---|---|
| Both have it | Keep it, with ours' kind. Merge the body as below. Anchor by ours if its block survives, else theirs, else rehomed |
| One has it, and it's new or its body differs from the base | Keep it. Anchor by that side, or rehomed |
| One has it, with the body unchanged from the base | The other removed it, so it's gone |

**Body when both have it:**

1. If ours equals the base, take theirs.
2. If theirs equals the base, take ours.
3. If they're the same, take either.
4. Otherwise keep both: `{ours}\n\nOn {theirs label}: {theirs}`.

**Moving an anchor.** A whole-block anchor stays if the block survives. A range in a block both sides had goes through that side's map, with `start` through `start()`, and `end` through `end()` but never before the new start. A zero-width anchor stays zero-width, so a footnote's marker doesn't widen. A range in a block only one side had stays as it is.

**Rehoming.** When a note's block is gone, it becomes a whole-block anchor on the nearest earlier block that survives, in the order of the side that had it (ours, if both did), or on the first merged block if there's none.

**The final list** is every kept annotation, then the new notes whose blocks survive, sorted by id with a stable sort, keeping the first of any with the same id. New notes come from ours' blocks in order, with each block's conflicts in text order, then from blocks only theirs has.

The document keeps the `id` of ours, and its `title` is by three-way.

## 5. Merging metadata

Merging metadata is separate from merging documents. Taking `later` and `earlier`:

- `title`, `author`, `created`, `published`, `tags` and `publish`: if the base equals later, take earlier, otherwise later. With no base, later wins.
- `words` and `accepted`: later's, followed by anything of earlier's it doesn't have.

Conflict copies and replaced files only take in `words` and `accepted` from the other copy (section 7).

## 6. Revision logs

SQLite can't survive file sync, because two machines write one file. So don't sync it. Each machine keeps its own database and appends to its own log, which has one writer, so a sync tool never sees a conflict. Revisions form a graph, as in git, and are addressed by content.

### 6.1 Layout

```text
garden/
├── .kame-logs                     turns on logs here and below
└── maritain/
    ├── essay.kame                 this machine's database. Not synced
    └── .essay.kame.sync/
        ├── talos-3f9a.log         appended to only by talos
        └── titan-81cc.log         appended to only by titan
```

**Turning it on.** A file named `.kame-logs`, normally empty, in the document's folder or any folder above it, turns on logs for the document. Its contents don't matter. It must sync, so every machine agrees. Each document's logs are in the hidden folder `.NAME.sync` beside it.

**The carrier's job.** Anything that copies files can carry logs, and logs ask very little of it:

- Copy `.kame-logs` and the `.sync` folders, and don't touch the logs' contents.
- Don't copy `*.kame` files in a logged folder, or their `-wal` and `-shm`. With Syncthing, that takes three lines in `.stignore` on every machine: `/writing/garden/**.kame`, `*.kame-wal` and `*.kame-shm`. `kame-sync enable FOLDER` prints them if it finds a `.stignore` there or above.
- Deliver a log by appending to it, or by replacing it atomically with one that extends it. A log that arrives late or out of order is fine, and an unfinished last line waits. Bytes changed before the read position, such as a same-length replacement, aren't seen until the document is reopened. A log replaced by a shorter one is read again from its start.
- Move `.NAME.sync` along with any rename, because logs are found by the file's name.
- Don't copy the app's config between machines, since the device name lives there, and two machines sharing it would be two writers on one log.

Only top-level `*.log` files are read. Readers ignore the log's name and take the device from each revision, so a carrier's conflict copy named `*.log` is harmlessly read as another log. Anything unreadable there, including a folder named `*.log`, fails the whole read.

**Device names.** A log's name is the device name plus `.log`, and it must have one writer. A name is the host's first label, lowercased, with anything but ASCII letters and digits turned into `-`, or `machine` if that's empty. Then comes `-` and 4 random hex digits. It's chosen once per machine and kept in the app's config. When shown to people, a final `-` and 4 hex digits are dropped.

### 6.2 A log line

Each line is one JSON object, followed by a line feed. Lines must be written whole and in one write, and a writer must start a new line first if the log doesn't end in a line feed, since a crash can leave part of a line there. kame doesn't fsync them. Example, wrapped here:

```json
{"id":"12d59870561bfca560505d422b9f7950",
 "revision":{"parents":[],"device":"talos-ab12","when":1000,
   "document":"00000000-0000-0000-0000-000000000001","title":"t",
   "order":["2285bd3af39e16f59630b13203e9f14d"],"annotations":[],
   "metadata":{"title":"","author":"","created":"","published":"","tags":[],"words":[],"accepted":[],"publish":false}},
 "blocks":[{"id":"00000000-0000-0000-0000-000000000010","kind":"Paragraph","text":"Hello.","spans":[]}]}
```

| Field | Meaning |
|---|---|
| `id` | The digest of `revision` |
| `blocks` | Blocks the revision uses that the writer hadn't seen in any log |
| `revision.parents` | Parent ids, sorted, so a merge's id doesn't depend on who made it |
| `revision.device` | The device that wrote it. Empty for a merge or checkpoint, which are the same revision on any machine |
| `revision.when` | The writer's clock in Unix seconds. For a merge, the later parent's. For a checkpoint, its op count |
| `revision.document` | The document's id |
| `revision.title` | The document's `title` |
| `revision.order` | The digests of its blocks, in order |
| `revision.annotations` | The document's annotations, whole |
| `revision.metadata` | Its metadata. Checkpoints carry defaults |

### 6.3 Reading

Read every `*.log` in the folder, in sorted order:

1. Ignore anything after the last line feed, because it's still being written.
2. Skip any line that won't parse, or whose `id` doesn't match its revision's digest as read back.
3. Index each block by its digest, also as read back, and each revision by `id`.

Reads can resume where the last stopped, past lines that failed to parse. A log that has got shorter is read from its start, which is harmless since everything is addressed by content. Readers don't check `revision.document` (section 9).

**Usable revisions** have all their blocks here and all their parents usable. A log can arrive before the one holding what it builds on, and then that revision waits.

**Heads** are usable revisions that no usable revision builds on, sorted.

**The shared ancestor** of two revisions is the greatest id among common ancestors that aren't a parent or further ancestor of any common ancestor. Where merges have crossed there can be several, and the choice only has to be the same everywhere.

**The label** for a revision is its device's display name. For a merge or checkpoint, follow the parent with the greatest `(when, id)` until one has a device, or use `another machine` if none has.

### 6.4 Writing

The `sync` key in the file's `meta` table holds the id of the revision this machine last settled on, called `last` here. To sync an open document with the logs:

1. Refresh from the logs. Forget `last` unless it's usable. It may not be in them, as after Save As, or a log it needs may have been cut short. A revision built on an unusable one couldn't be read either, so what's typed would be left out of the document.
2. If there's no `last`, use any head with the same document digest and metadata.
3. If there's still none, **write checkpoints** (below) and use the latest.
4. If the document or metadata differ from `last`, append a revision with `last` as its parent.
5. While there are two or more heads, take the first two. Merge them against their shared ancestor (section 4), or with no base if they have none, using their labels and `when`s. Merge their metadata (section 5) the same way, with the greater `(when, id)` as later. Append the result with both as parents, no device, and the later `when`. If that revision already exists with all its blocks here, nothing is written. If it's there but waiting on blocks in a log that hasn't arrived, write the line again with the blocks missing here, which this machine has just worked out. Repeat.
6. If there's no head at all, fail.

Appending writes only blocks that no log here has, and does nothing if the id is already there and no blocks are missing.

The app skips all this if no log has changed size and the document and metadata match the last sync.

**Checkpoints** give two machines a shared past from the start. Undoing back through the applied ops, take the document's state at each op count divisible by 50, and at every count after the last of those. Write them oldest first, each the parent of the next, with no device, default metadata and the op count as `when`. Machines with the same history write the same ones, so a merge has an ancestor near where they really parted. The interval is part of what machines agree on. If the history can't be undone any further, there are no checkpoints before that point.

### 6.5 Applying the result

Writing a merge's result to the open document:

- If the merged document differs from what's open and adds annotations, take a snapshot labelled `before merging from another machine`, and don't merge if that fails. Then replace the document with one `ReplaceDocument` op, as its own undo step.
- If it differs without adding annotations, replace it too, but successive arrivals with no edit between them join into one op.
- Carry the caret by the text its block shares at each end with the new text. Inside what changed, it goes to the start of the change.
- If the metadata differs, save it. It's not an edit.
- Set `sync` to the new head.

Skip the sync if the session can't save, or if its file has been replaced. Something else is asking the person about that.

### 6.6 Building from logs

A document with logs and no database can be built, even with no `.kame-logs` above it, since it would otherwise open blank where the logs have the text: write to `NAME.building` beside it, with the first head's document and metadata and `sync` set to that head, then remove any `-wal` and `-shm` and rename into place. If the logs hold no whole document, write nothing. Other heads merge once it's open.

### 6.7 Timing

While a logged document stays open, the app looks at its logs every 3 seconds. It syncs only after 5 seconds without typing, when focus changes, or on returning after a gap of over 10 seconds between frames, since the machine slept or the window was hidden, and not with a dialog or a note open. It also syncs, with none of those conditions, on leaving the document, before opening a file and on exit. An untouched new document writes no logs. A document with logs but no `.kame-logs` above it gets a warning, because it may have stopped syncing.

### 6.8 A newer version in the logs

An older build must notice lines it can't fully read, rather than quietly falling behind. A line is from a newer version if it parses but has a field that's gone after reading and writing it again, or if it fails to parse on a kind this build doesn't know. Half a line, a damaged line and an older line lacking fields added since don't count.

Having noticed, a sync still writes what's on the screen as a revision, so nothing typed is lost, and merges what it can use. Then the document goes read-only, saying that it has changes from a newer version of kame, until kame is updated.

## 7. Conflict copies and replaced files

These are for a tool like Syncthing carrying the `.kame` file itself, and the app looks for them in logged folders too. There's no shared history to read here, so the base is found from the op logs:

1. Read each file's applied ops, leaving out anything undone.
2. Count how many ops they share from the start.
3. Start from one side's document and undo its other ops. If compacting has left nothing shared, this gives an older document than the true one, which means more conflicts and no lost words. If the ops can't be undone that far, merge with no base. Stopping partway would count this side's later words as shared, and the other side as having cut them.

**Conflict copies.** kame looks every 5 seconds for files beside the document named `STEM.sync-conflict-*.kame` that hold the same document id. To merge one:

1. The screen is one side, with `when` 1 and the label `this machine`. The copy is the other, with `when` 0 and the label `the copy from YYYY-MM-DD HH:MM`, if its name says so, otherwise `the other copy`. So the words left in the text are the ones on the screen.
2. If the merge changes anything, take a snapshot labelled `before merging a conflict copy` first, and don't merge if that fails. Then replace the document with one undo step.
3. Take in the copy's `words` and `accepted`.
4. Rename the copy by adding `.merged`, so it's no longer offered. It stays, since undoing the merge shouldn't leave it the only place those words are.

**A replaced file.** If a new file arrives over the open one, kame stops writing ([document format, section 1.6](document-format.md)) and asks. To merge, the file must still be there with the same document id. The screen is one side, with `when` 1 and the label `this machine`, and the file is the other, with `when` 0 and the label `the other machine`. Open the file, and if the merge differs from it, take a snapshot labelled `before merging with the copy on this machine`, then replace the document with one undo step. Take in the old session's `words` and `accepted`.

**Command line.** `kame-sync merge ONE OTHER OUT` merges two files into a new one. A file doesn't say when it was written, so there the longer history counts as the later. It writes ONE's metadata whole and takes nothing from OTHER's.

## 8. Notes are private

Everything a merge adds is a `Note`, which never publishes. A merged document can hold notes for the person to settle, and an implementation must show them rather than resolving them silently.

## 9. Known weak points

These describe the code as it stands, and aren't rules to copy.

- **Nothing checks `revision.document`.** Logs from a different document at the same name merge in, and the result takes the id of ours.
- **A sync stops silently if the logs can't be read at open.** `Logged::open` returns nothing, and the app shows no message.
- **A newer build's revisions stop an older one following.** Digests and ids are recomputed from what a reader parsed, and readers drop fields they don't know. So when a newer build writes a field an older one doesn't know, the older one's digest differs: with a new field in a block, the line is read but the revision never becomes usable, and with one in metadata, the line is skipped. Everything built on that revision waits too. Section 6.8 makes that visible, but the older machine can't follow until it's updated.
- **A new field must be left out when empty, or old revisions break.** If a new field were always written, a reader would add it when re-serializing old lines. In a revision or its metadata, the ids would stop matching and the lines would be skipped. In a block, the digest would change and the revision would stay unusable. `Metadata` writes all its fields, so a new metadata field needs a `skip_serializing_if`. Any change to how the JSON is written has the same effect. Golden tests at the end of `crates/kame-sync/src/revlog.rs` pin the bytes written today for a revision and its blocks, so a field added without that fails them.
- **Sentences are English-shaped.** Abbreviations such as `e.g.` end a sentence if a space follows, which only makes conflicts coarser or finer and loses nothing. Text without those marks, such as Chinese with `。`, counts as one sentence until a line feed.
- **Clocks decide which words stay in the text.** The later `when` becomes ours. The other words are kept in a note either way, so a wrong clock changes which words are in the note and loses nothing.
- **Logs only grow.** Nothing compacts or removes them yet.
- **Whole documents in the undo log.** Every merge stores `ReplaceDocument` with both versions, so frequent arrivals grow the file, even with joining.
- **`when` for a checkpoint is an op count, not a time.** Comparing it with a real time always makes the checkpoint the earlier one, which is the intended result, but `when` doesn't mean one thing.
- **Device names come from the `hostname` command.** Where it's missing, the name falls back to `machine` plus the random part. Only the random part keeps two machines' logs apart, and it's 4 hex digits.
- **Only Syncthing's conflict copies are recognised.** Other tools name theirs differently, so those copies are listed as separate documents in a notebook and never offered for merging. Revision logs don't depend on this.

## 10. Test vectors

Produced by `kame-sync` at this commit. Check them with any SHA-256 tool:

```sh
printf '%s' '{"id":"00000000-0000-0000-0000-000000000010","kind":"Paragraph","text":"Hello.","spans":[]}' | shasum -a 256 | cut -c1-32
# 2285bd3af39e16f59630b13203e9f14d
```

| What | Value |
|---|---|
| The block above | `2285bd3af39e16f59630b13203e9f14d` |
| A document with id `…0001`, title `t`, that block and no annotations | `756ca12df6414fbbc2ef42400c6690e5` |
| The revision in section 6.2 | `12d59870561bfca560505d422b9f7950` |

A merge with a conflict. The base block is `One two.`. `talos` wrote `One three.` with `when` 5, and `titan` wrote `One four.` with `when` 9. Passing them in either order gives:

```json
{"id":"00000000-0000-0000-0000-000000000001","title":"t",
 "blocks":[{"id":"00000000-0000-0000-0000-000000000010","kind":"Paragraph","text":"One four.","spans":[]}],
 "annotations":[{"id":"c594d8a8-4590-38ba-9fac-9f0ae92136e4","kind":"Note",
   "anchor":{"block_id":"00000000-0000-0000-0000-000000000010","start":0,"end":9},
   "body":"On talos: One three."}]}
```

A block cut on one side and edited on the other. The base has `keep` (`…0010`) and `x` (`…0011`). `talos`, with `when` 5, cut `keep`. `titan`, with `when` 9, changed it to `keep edited`. The block stays, in the later side's words, with a note:

```json
{"id":"ca559c82-e13e-0a28-7867-6d944a8f308d","kind":"Note",
 "anchor":"00000000-0000-0000-0000-000000000010",
 "body":"Cut on talos, kept because it changed on titan."}
```
