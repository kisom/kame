# Sync between one person's machines — design

Date: 2026-11-03
Status: accepted 2026-11-03, including `sha2`. A working proof of concept is in `crates/kame-sync`. Written overnight without
a chance to ask, so section 10 lists every decision made on your behalf. Section 13 is a second pass on what stage 2 needs.
Stage 2 is built, and in trial on `notebook-test`, with section 14 saying what building it changed.

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
2. **Revision logs in the app**, opt-in per folder, as section 13 sets out.
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
- **Before this goes in the app:** see section 13 for reading logs, trimming and unique device names.
  Nothing here touches the app.

## 12. Trying it

```sh
cargo run -p kame-sync -- merge A.kame B.kame OUT.kame   # never changes A or B
cargo run -p kame-sync -- trial DOC.kame                 # prints counts only
cargo run -p kame-sync -- study DOC.kame                 # what logs would weigh
```

## 13. Second pass: what stage 2 needs

The test throughout is how easily each decision can be undone. Essays are what's at stake, and this software is days old, so
the lack of any conflict so far says nothing.

**Opting in is by folder, not by document.** Section 8 used to say per document with `*.kame` in `.stignore`, which contradicts
itself: that line stops every document syncing, including those not opted in, and a document could silently stop moving between
machines. So what's logged and what's ignored have to be the same set, which means a folder:

- A file named `.kame-logs` in a folder turns on logs for every document under it, found by looking up through parent folders, as
  a notebook is found. It's empty and it syncs, so every machine agrees.
- That folder's databases are ignored with one line per folder in `~/data/.stignore`, such as
  `/writing/notebook-test/**.kame`, added by hand on each machine. Syncthing doesn't sync that file, though it can `#include` one
  that it does, which could make this one edit.
- A notebook is then simply a folder that may have this file. The sync design doesn't need a notebook, and a notebook doesn't
  need logs.
- If the line is missing on a machine, its database keeps travelling too. That's today's situation, with `file-guard` and merging
  as the net, so nothing is lost, but it's noisy.
- If the file is missing and the line isn't, documents stop moving. A document with logs and no file above it says so in the
  status row, since it may have stopped syncing. A document that never had logs in that folder can't be told apart, so `kame-sync
  enable` prints the line to add, and removing the two together is the way back.

**The database stays beside the document.** The alternative in section 10, a `.kame` folder with the database in
`~/.local/state`, needs no ignore rule, but it changes what a `.kame` is, and can't be tried on one folder and taken back.
Keeping it beside the document is purely additive: logs are added, and nothing about a `.kame` changes.

**Going back.** Remove the `.stignore` line, and the databases sync again, as conflict copies at worst. Merging two databases with
no shared history was tried, and added no notes and lost no words in the three cases below. Then delete `.kame-logs` and the
`.sync` folders.

| Case | Notes added | Words lost |
|---|---:|---:|
| Same text, separate histories | 0 | 0 |
| Each edited a different block, from the same revision | 0 | 0 |
| Built from different revisions, with no edits since | 0 | 0 |

Not tried: both built from different revisions and then edited, which should give notes where they overlap.

**What opting in gives up.** The database no longer travels, so snapshots and undo history stay on the machine that made them.
Every revision's text is in the logs everywhere, so nothing written is lost with a machine, but going back to an old revision
needs a command that doesn't exist yet. Snapshots made before opting in stay in each machine's copy of the database.

**A document with only logs.** A fresh machine has `X.kame.sync/` and no `X.kame`. It's a document all the same, listed by the
title in its latest revision, and opening it builds the database, as section 7 says. For a notebook:

- The list of documents counts `X.kame.sync/` with no `X.kame`.
- A database can lag behind its logs, when another machine wrote while this one's copy was closed. So the database records how
  much of each log it has seen, and comparing that with the logs' sizes finds the ones behind.
- On opening a notebook, a background pass brings those up to date: building what's missing and merging what's new, into the
  ignored database. Search then reads head rows as the notebook design says, and those it hasn't reached yet are read from the
  logs. This writes only to databases, and never during a search.

**Reading logs is fast enough.** Measured on the real histories in `notebook-test`. The 7 seconds in section 11 was the prototype
reloading every log for each of 400 revisions, and it isn't the cost of a read. Each log read here is well under the time a person
would notice. A notebook can't read all its logs for each search, which is why the databases are kept up to date.

| History | Revisions | Log | Database | Reading the log | Finding heads |
|---|---:|---:|---:|---:|---:|
| 2,644 words, 4,102 ops, every 50 ops | 84 | 266 KB | 815 KB | 2.6 ms | 0.2 ms |
| The same, every 10 ops | 411 | 1.1 MB | 815 KB | 4.2 ms | 0.4 ms |
| 1,356 words, every 50 ops | 48 | 124 KB | 356 KB | 0.5 ms | under 0.1 ms |
| 674 words, every 50 ops | 64 | 125 KB | 397 KB | 0.6 ms | 0.1 ms |

At sizes no essay reaches, with made-up edits to the longest of these:

| Revisions | Log | Reading | Heads, now | Heads, fixed | Ancestor, now | Ancestor, fixed | One new revision, keeping logs in memory |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 400 | 1.6 MB | 5 ms | 2.7 ms | 0.5 ms | 8 ms | 0.2 ms | 2.7 ms |
| 2,000 | 8.2 MB | 26 ms | 97 ms | 3.6 ms | 254 ms | 1.1 ms | 14 ms |
| 10,000 | 46 MB | 137 ms | not run | 30 ms | not run | 8.8 ms | 101 ms |

Reading 100 documents' logs of 200 revisions each, with their heads, took 305 ms. That's about 3 ms a document, against under
0.1 ms for a head row.

- **Two fixes are needed.** `usable` and `shared_ancestor` each become linear. In a scratch copy, both gave the same answers as the
  prototype on every history above up to 2,000 revisions, which matters since every machine has to choose alike. None of those
  histories has crossed merges, so a test has to cover that.
- **The app keeps a loaded log in memory** while a document is open, and reads only what's new.
- **Revisions on a pause, at about one per 50 ops.** Every 10 ops makes a log bigger than the database. Most of a log's weight is
  a whole paragraph repeated each time it changes, and the list of notes in every revision.

**No trimming yet.** A log at this rate stays smaller than its database. Trimming would have to keep the shared ancestor that a
machine long offline needs, so it isn't simple, and it can be added later without changing the format of what's already written.

**Device names can't collide.** Each machine picks its name once, as its host name and four random characters, and keeps it in
`config.json`: `talos-3f9a.log`. Notes still say "On talos". Reinstalling makes a new log, which is safe, whereas two machines
writing one log would break the premise.

**How often conflict notes appear stays unknown.** It can't be learned until this is in use, and being opt-in by folder is what
makes that safe. Try it on `notebook-test` first, then on one real folder, and see.

### Stage 2, in order

1. In `kame-sync`: the two linear fixes, reading only what's new, and device names. Built.
2. `.kame-logs`, with the app writing revisions and merging when a document is open. Built, and in trial on `notebook-test`.
3. A database built from logs on opening or following a link, and kame reporting logs with no marked folder. Built.
4. A real folder.

The background pass, and the database recording what it has seen of each log, wait for the notebook. `kame-site` reading from
logs follows once a published folder opts in.

## 14. What building stage 2 changed

**Machines need an ancestor from the start.** Two machines opting in separately had no shared revision, so any edit looked like a
conflict, and even a machine's unchanged text did when a document's history began empty. So a machine's first sync now writes
checkpoints through the document's history: every 50 ops, then every op after the last of those. They hold no device or time, so
machines sharing a past write the same revisions, and two that were in step when they opted in share one right up to that point.
This costs each log one pass through the history, and doubles a log's weight if two machines write it before seeing each other's.

**Sentences both sides have are agreed.** A base can be older than where two machines parted, or missing. Whole sentences both have
at the start or end of a block now count as agreed even if the base lacks them, in `merge3`, which helps every merge. An edit inside
a shared sentence is still a conflict.

**Live test.** Over real Syncthing between Talos and Titan, on `notebook-test`, through the command line:

| Step | Result | Agreed after |
|---|---|---:|
| First sync of three documents already in step | no notes | at once |
| Edits to different paragraphs at one moment | merged, no notes | 26 s |
| Edits to the end of one paragraph at one moment | one identical note on both | 68 s |
| Database deleted on Titan, then rebuilt from logs | same text, and Talos kept its own | 9 s |

The databases then differed on the two machines and stayed apart, with no conflict copies. The times are mostly Syncthing's and the
test's own polling. Logs came to 348 to 556 KB, about twice the single-machine weight, from both machines having written their
checkpoints at once.

**In the app.** With a script playing the other machine: words arrived on a pause and said where from, nothing landed during typing,
and the last words reached the log on closing. Reopening a file with news waiting showed the news.

**Still unknown.** How often conflict notes appear in real writing, long offline drift, and Titan's app in daily use.
