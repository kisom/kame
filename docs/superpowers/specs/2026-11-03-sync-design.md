# Sync between one person's machines — design

Date: 2026-11-03
Status: accepted 2026-11-03, including `sha2`. A working proof of concept is in `crates/kame-sync`. Written overnight without
a chance to ask, so section 10 lists every decision made on your behalf.

## 1. Purpose

kame is for one person on a few machines, each drifting offline for a while. The goal is to write
on any of them and have nothing lost and nothing to think about, without running a server and
without giving up Syncthing. This is not Google Docs. There is no second author and no cursor to
watch.

## 2. What is happening today

Syncthing carries `~/data`, including `.kame` files as they are written.

- **The sidecars travel too.** On 2026-11-03 at 01:25, Talos had `AgainstBrainrot.kame-wal` and
  `-shm` with fresh timestamps and no kame running there, and they vanished when the document
  closed elsewhere. Nothing ignores them: Titan's `.stignore` has no kame lines, and Talos has none.
- **A mispaired log overrides the file.** SQLite has no way to tell which database a `-wal` belongs
  to. In a test, with a stale `-wal` left at the path, an incoming copy that said "theirs" read back
  as "mine". Later checkpoints would then write those pages into the new file.
- **SQLite keeps that log there on purpose.** When a database has moved, closing skips the
  checkpoint and leaves `-wal` behind, so a quit or crash leaves the trap for the next open.

`file-guard` handles this for a document open during the replacement. It cannot help when a `-wal`
and its database arrive from different moments, so **every machine's `~/data/.stignore` should have
these two lines now**, whatever becomes of the rest of this design:

```
*.kame-wal
*.kame-shm
```

A file then reaches other machines as of its last checkpoint, usually when it closes.

## 3. Principles this applies

- Nothing is ever lost. If in doubt keep both.
- The page stays a page. No modal conflict screens and no markers in the text.
- The author decides prose. Software decides bookkeeping.
- One-way layering: this needs nothing from the GUI or the typesetter.
- It is a personal tool, so no accounts, no server, no protection from a hostile machine.

## 4. Options, and why states rather than ops

The notes in `followups.md` assumed the op log was the thing to sync. Having looked, I recommend
against that.

| Option | For | Against |
|---|---|---|
| A CRDT (Yjs, Peritext) | Character-exact, well studied | Replaces the op log and model, and costs metadata forever. Built for live co-editing, which you do not do |
| Eg-walker | Keeps positional ops, as kame does | Needs a graph of ids and parents on every op. `coalesce` and `compact_history` rewrite ops, which is exactly what it relies on staying put |
| Replaying one log over the other | Small | Ops have no identity, so compaction on one machine breaks the shared prefix |
| **Merging states, by block and sentence** | Needs no new data in the log. Coalescing and compaction cannot affect it. Pure function, so easy to test | Sees less than ops do, such as text moved between blocks |

The deciding fact is that blocks and annotations already have ids that survive editing. That turns
most of a merge into bookkeeping, and leaves text only where both sides changed one block.

## 5. The merge

`merge(base, one, other)` is a pure function, in `merge.rs` and `text3.rs`.

**Blocks.** Changed on one side: take it. New on one side: keep it after whatever it followed. Cut
on one side and untouched on the other: it goes. Cut on one side and edited on the other: it stays,
with a note saying so.

**Text, when both changed a block.** The unit of conflict is the **sentence**. Git merges by line
because programmers edit lines. Word-level merging of prose can succeed and still make nonsense:
"the quick brown" edited to "a quick red" and to "the slow brown" merges to "a slow red". So edits
in different sentences merge silently, and edits on both sides of one sentence conflict, unless they
are the same edit. In a code block a line ending also ends a sentence, so it merges by line.

**A conflict becomes a note.** kame is a marginalia editor, and the margin is where a writer already
keeps a second thought. The later version stays in the text, and the other goes in a note on that
sentence, for example "On titan: The cat sat down." There are no markers in the prose, the document
stays editable, and resolving it is ordinary writing: use the words or delete the note.

**Formatting and notes.** Offsets are carried through the merge, so notes and spans follow their
words. Bold removed on one side stays removed. Formatting on words that lost a conflict is dropped
and cannot spread over the words that won. A note's body edited on both sides keeps both.

**Same answer everywhere.** The later side goes first whichever way round the arguments come, with
the digest settling a tie, and new notes take ids from their content. Two machines merging
separately produce identical bytes. Clock skew can only change which version sits in the text, and
never loses the other.

## 6. Revision logs

SQLite cannot survive file sync, because two machines write one file. So do not sync it.

```
AgainstBrainrot.kame            this machine's database, ignored by Syncthing
AgainstBrainrot.kame.sync/
    talos.log                   appended to only by talos
    titan.log                   appended to only by titan
```

**Each log has one writer, so a sync tool has nothing to conflict over.** Syncthing moves them as it
does any file, and rsync or a USB stick would do as well. Phobos can carry them with no kame on it.

A revision is one JSON line: parents, device, time, title, annotations, and an ordered list of block
digests, with the text only of blocks not seen before. Ids are digests of content, as in git. A line
cut off in transit fails to parse and is ignored until the rest arrives. A revision whose parents or
blocks have not arrived yet waits.

**Convergence needs no coordination.** When a folder holds several heads, each machine merges them
in sorted order, against their latest shared ancestor. A merge's id depends only on its parents and
result, so machines merging separately write the same revision, the duplicates collapse on reading,
and they have converged. This is what makes a server unnecessary.

## 7. In the app (not built)

- Write a revision on a pause in typing, on losing focus, and on close. Local editing is untouched:
  the database still takes every keystroke, in one transaction, as now.
- Look at the folder every few seconds. If something new is there, write a revision, merge, and
  apply the result at the next pause, never mid-word. Keep the caret where it was, through the same
  offset maps. A quiet word in the status line: "merged from titan", or "2 notes to look at".
- **Undo.** A merge is one undo step, using a new invertible op holding the document before and after,
  so the invariants hold. History stays linear and per machine. Undoing a merge just writes the next
  revision, and the merged text does not come back, which `logs.rs` tests.
- A snapshot is taken before any merge that adds a note.
- Metadata (`published`, tags and so on) goes in revisions and merges field by field. Images stay as
  they are: files beside the document, already carried by Syncthing.
- `kame-site` builds a document from its logs when there is no database, in memory, still leaving
  nothing behind. It should warn about unresolved merge notes, since notes highlight in exports. That
  suggests a distinct annotation kind for them.
- A new machine opening a path with no database but a `.sync` folder builds the database from it.

## 8. Stages

Each stage is useful alone.

0. **Now:** land `file-guard` and add the two `.stignore` lines.
1. **Merge conflict copies.** Built. kame notices a `.sync-conflict-` file beside the document and
   File > Merge Conflict Copies merges it, and a file replaced under an open document offers Merge
   Them. Words on the screen stay in the text and the other version goes in a note. Each merge is one
   undo step after a snapshot, and a merged copy is renamed `.merged`, not deleted. No format change.
   `kame-sync merge A.kame B.kame OUT.kame` does the same from the command line.
2. **Revision logs in the app**, opt-in per document, with `*.kame` added to `.stignore`.
3. **The notebook.** The same logs per document give a binder its sync with nothing further.

## 9. Evidence

From the proof of concept. All of `cargo test -p kame-sync` passes.

**Real history, with a known right answer.** `kame-sync trial` takes a real stretch of a document's
log, deals each changed block's edits to one of two imaginary machines, merges, and compares with what
really happened.

| Document | Ops | Rounds | Text identical | Same content, adjacent new blocks in another order | Content wrong | Spurious notes |
|---|---|---:|---:|---:|---:|---:|
| CommandingHeights | 2345 | 413 | 275 | 138 | 0 | 0 |
| MultiKame | 3120 | 403 | 311 | 92 | 0 | 0 |

The order differences are two machines each adding a paragraph at one spot, where nothing says which
comes first.

**Random divergence.** 400 runs of random edits through kame's own commands, with multibyte text: every
word typed on either side survived in the text or a note, either argument order gave identical
documents, anchors and spans stayed valid, and exports stayed deterministic. A further 60 runs with
three machines and random delivery order all converged on one revision.

**Real Syncthing,** in a throwaway folder in `~/data`, since deleted. Talos and Titan and Straylight
(Linux and macOS) edited at the same moment.

| Step | Agreed on one revision after |
|---|---|
| Each begins the document separately | 69 s |
| Three edit different paragraphs in the same second | 18 s |
| Two edit one sentence | 17 s, with one identical note on all three |
| Several edits on one machine, one on another | 25 s |

No conflict copies appeared, and Phobos, with no kame, received every log. Most of that time is
Syncthing's 10 second watcher delay and the test's own 8 second polling.

**Weight.** Full history at one revision per 50 ops: 122 KB for CommandingHeights against a 356 KB
database, and 125 KB for MultiKame against 397 KB.

## 10. Decisions made without you

| Decision | If wrong |
|---|---|
| Merge states, not ops | The largest one. Text moved between blocks is seen as a cut and an insertion. Reversible, since logs and merge are separate |
| Conflict per sentence, not word | More notes than strictly needed. One function to change |
| Later version in the text, the other in a note | Purely which goes where |
| `X.kame.sync/` beside the file, with the database ignored | Needs `.stignore` on every machine. The alternative is a `.kame` folder with the database in `~/.local/state`, which needs no ignore rules but changes what a `.kame` is |
| `sha2` for digests | A new dependency, which AGENTS.md says needs a spec change. This is that request, and only `kame-sync` uses it so far |
| A prototype crate linked into nothing | None: delete `crates/kame-sync` and nothing else notices |

## 11. What this does not show

- **Sentence conflicts in real writing.** The real-history trial deals out whole blocks, so it never
  produces one. Those paths have only synthetic tests. How often you would see notes is unknown, and
  it is the main thing to learn by using it.
- **Long offline drift.** The live test diverged for seconds, not days. Random tests stand in for that.
- **A paragraph split on one side and edited on the other** loses nothing, but the edit lands in a note
  rather than in place.
- **Order of two new blocks at one spot** is settled silently and may be wrong. A note might be better.
- **Crossed merges** with several shared ancestors use one of them. It agrees everywhere, but can raise
  a conflict again.
- **wntrmute** was unreachable by ssh, so it took no part.
- **Before this goes in the app:** reading logs and finding ancestors is quadratic in history (7 s for
  400 revisions in the study, mostly from re-reading). Logs need caching and eventually trimming. Device
  names must be unique, since a log with two writers breaks the whole idea. Nothing here touches the app.

## 12. Trying it

```sh
cargo run -p kame-sync -- merge A.kame B.kame OUT.kame   # never changes A or B
cargo run -p kame-sync -- trial DOC.kame                 # prints counts only
cargo run -p kame-sync -- study DOC.kame                 # what logs would weigh
```
