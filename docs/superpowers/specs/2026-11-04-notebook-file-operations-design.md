# Renaming, moving and deleting documents in a notebook

Revises sections 8 and 10 of `2026-11-03-notebook-design.md`, which left the Move command and the clean-up of what it
leaves on other machines for later.

## Why

Kyle made a document by accident and had no way to get rid of it from kame. Right-clicking it in the notebook browser was
the first thing Kyle tried, and rename, move and delete are the operations Kyle expects to find there. The real notebook
(`~/data/writing/essays/`) syncs with revision logs, so each of these has to hold on the other machines too. A delete
that comes back isn't a delete.

## Goals

- Right-clicking a document in the browser offers Rename…, Move… and Delete….
- Renaming or moving a document leaves every link to it working, in every document kame can edit.
- All three hold across machines in a folder with revision logs.

## Non-goals

- Folders, and files that aren't documents (`Kind::File`: papers, images). Their rows get no menu.
- A trash, or undoing a delete. Kyle's call: "If I delete the wrong file, that's on me."
- Undoing a move. The link rewrites are ordinary undo steps, each in its own document, but the file move isn't one.
- The command palette the notebook spec imagined. The browser's menu is the way in.
- Redirects for a published page whose URL changes with its path. Published URLs follow the folders, as they do now.
- Repairing links on delete. They stay, and following one offers to create the document, as a stale link does now.

## Anti-goals

- A background pass that loses words. Everything the clean-up does on another machine is a rename, never a delete.
- Half-finished moves that leave links wrong with no word about it. Whatever couldn't be changed gets listed.

## Decided by Kyle

- The menu lives on browser rows, with rename, move and delete.
- Delete is permanent.
- Links are rewritten on a rename or move, and it may be a little slow, since it's rare.
- Renaming or moving over an existing file is an error. Delete the other one first.
- A rename changes the title.
- The work happens in a worktree.
- The clean-up on other machines is part of this work, since the real notebook has logs.

## Behaviour

Unless listed above, what follows is the designer's call, which Kyle read in summary and accepted.

### The menu and dialogs

Each document row in `show_browser` gets a context menu with Rename…, Move… and Delete…. The rows of the jump list and
search don't.

**Rename and Move** open one dialog, with one text field holding the document's path from the notebook's root, such as
`maritain/materialism.kame`. They differ only in what's selected when it opens: Rename selects the file's name without its
extension, and Move selects the folders. A document at the root has no folders to select, so what's typed goes in front of its name,
and without a `/` that's a rename, and the Move dialog says to type a folder and `/` before the name. Enter or the button
confirms, and Escape cancels. A refusal shows in the dialog and leaves it open.

A new path is refused if:

- it leads outside the notebook, or isn't where a document can be (`Notebook::holds` and `in_documents_area`: not under
  `static/`, `images/`, `theme/`, `output/`, and with no part starting with `.`)
- something is already there, whether a file or a log folder. Kyle's call: this is an error, and the way forward is to
  delete the other file first, which the refusal says. There's no offer to replace it. A case-only rename on a
  case-insensitive disk is allowed (`same_file` says it's the same document)
- it contains `.sync-conflict-`
- it changes the extension. A missing extension gets the document's own, so `.md` stays `.md`.

Folders it names are created. Folders left empty stay, since the browser doesn't show them.

**Delete** asks once: "Delete *title*? This can't be undone." with Delete and Cancel. No more than that.

### Deleting

Removed: `X.kame`, its `-wal` and `-shm`, `.X.kame.sync/`, and any conflict copies and merged or retired copies of it, which
are hidden from the browser and would otherwise be left with nothing to belong to. A `.md` document is just the file.

A retired copy is told by its document's id as well as its name, as a conflict copy is, since a path can be reused. If the old
`X` was retired to `.X.retired.kame` holding words that never synced, and a new, unrelated `X` was made, deleting the new one
mustn't take the old one's only copy. A retired copy of another document, or one that can't be read, stays. A document with
only its logs has its id from their head, and if that can't be read, retired copies are left alone.

A stranded database has no logs of its own, so whatever is at its log folder stays, as it may be another document's. Those
logs keep its row in the browser, where they list as a document with only its logs, so `delete_document` returns whether it
left logs, and the app says so: "deleted *title*; the logs at that path may be another document's, and were
left". Without that a person reads the row as the delete not having taken, and deletes again, which with no database to say
otherwise removes the logs, on every machine.

If it's the open document, the app first syncs and leaves it: it goes to the notebook's contents page (`Notebook::index`)
if that isn't the one being deleted, and otherwise to a new anonymous document. The window's other memory of the path goes
too: the back stack (`BackStack::forget`), the recent list, `last_document` and any `fresh` entry.

The root `Notebook.kame` can be deleted like any other document. Home then says the notebook has none, as it does now.

A document another kame has open is refused, with nothing removed, and the app says "NAME is open in another kame window". So is
one with a conflict, retired or moving copy another kame has open, naming the copy, as those go with it. Their locks
(`kame_core::lock`) are held while they go, so that nobody opens one halfway. A document open in this window has been left first,
which lets go of its lock, and a copy of it open in this window is refused: "NAME is open in this window".

### Renaming and moving

1. **Refuse** if the open document is the one moving and has a failed save, or if it's read-only from a newer version.
   Refuse too, before any file moves or any link is rewritten, if another kame has the moving document open, a copy of it
   that would be renamed with it, or any document whose links the plan would rewrite: "NAME is open in another kame window",
   NAME from the notebook's root, for the first of them. A copy open in this window is refused as "NAME is open in this
   window". By then the plan may have made the new path's folders and synced documents behind their logs, which changes
   nothing a person wrote.
   Their locks are taken here and held until the move is done, so that none is opened halfway, and the moving document's
   lock holds across its rename, since a lock is for the file. The document this window has open is its own, and so is
   the one it's moving, whose lock it keeps while it leaves it. The plan's sync of a document behind its logs skips one
   another kame has, which is refused here.
2. **Plan, before anything moves.** Read every document in the notebook and work out which links lead to the old path and
   what each should become. A closed document's database can be behind its logs, so a document with logs is read from both,
   and one whose logs have such a link is opened and synced first, then planned from the result. Links arriving later from
   a machine that hadn't heard of the move can't be helped, and `follow` treats them as any stale link.
3. **Move the files.** `X.kame`, `-wal`, `-shm` and `.X.kame.sync/` move together, with conflict copies and merged or
   retired copies of it, told as a delete tells them, renamed to follow. A retired copy of another document stays behind under
   its old name. If the document is open, the app syncs it, leaves it, moves it, and opens it again at
   the new path with its caret and scroll, so that no SQLite connection is open across the rename. A failure here leaves
   the rest undone and says so.
4. **Rewrite the links** in each document in the plan. A failure in one doesn't stop the others.
5. **Rewrite the moved document's own relative links and image paths,** if it changed folders.
6. **Change its title,** if its file's name changed (below).
7. **Report.** A short window saying how many documents changed, and naming any that couldn't be, with why. A plain notice
   if there's nothing to name.

The window's memory of the old path (back stack, recent list, `last_document`, `logged_path`) follows to the new one, and
the listing and what-links-here are refreshed. It may block the window while running. No background thread is needed.

**Which links.** A link counts if `Notebook::follow` from its document gives `Follow::Open` of the old path, so it finds
what following finds:

- `SpanStyle::DocumentLink` and `SpanStyle::Link` in any block, and an image block's `link`. (`linking_to` looks only at
  `DocumentLink`. The site's `notebook.rs` already treats a relative `Link` as leading to a document, and this does too.)
- However it's written: relative, with no extension, a folder for its `Notebook.kame`, `~/…` (File > Link to Document
  writes these) or absolute. The new target keeps the form: `~/` and absolute stay so, and anything else is written
  relative. Any `#Heading` is kept as it was.
- A folder link to a `Notebook.kame` that's moved or renamed becomes a link to the file, unless it's still its folder's index.

**How each document changes.** Through a new `Session` method that builds the changed `Document` and applies it with
`replace_document`: one `Op`, one undo step, text and anchors unchanged. A closed document is opened as a `Session`, built
from its logs first if it has none (`build_from_logs`), changed, synced to its logs if the folder has them
(`Logged::open` and `sync`), and closed, so that the change travels without waiting to be opened. The open document is
changed through the app's own session.

**What's left alone, and listed:** `.md` and `.markdown` documents, which kame imports rather than edits, and documents from
a newer version of kame, with "it was written by a newer version of kame; update kame to change it", open or closed. One
from a newer version is read as far as this build can, and listed even when nothing it can read links here, since what it
can't read may, with "it was written by a newer version of kame, so it may link here in parts this kame can't read;
update kame to change it". So is one whose logs hold a revision from a newer version, which the plan reads anyway. A
refused change to a document with only logs still builds its database from them first. A document that can't be read isn't known to link at all and isn't listed, as with what-links-here.

**The moved document's own links** have one catch: `kame_core::links::retargeted` turns `#Heading` into a path, as is right
for a paste, and a moved document's `#Heading` still means itself. So it's used for everything except a link with no
path. A link in it to its own old name goes to its new name. An image path is handled by `Session::rehome_images`, which
leaves one it can't find as it was.

**The title.** A document starts with its file's name as its title, stored in `Metadata::title` and `Document::title`, and
most of kame shows that rather than the file's name. So when the file's name changes, each of those still equal to the old
name without its extension becomes the new name without its. A title the author has set to something else stays, since
a change to metadata can't be undone. A move to another folder under the same name changes nothing. It's written
along with the move and reaches the logs with it. A link's words are left as they are. `.md` documents are left alone,
since their titles come from the file.

### The other machines

In a folder set up as the sync spec says, Syncthing carries logs and not databases (a `**.kame` line per logged folder). So
after a move or delete on one machine, another holds a database with no logs beside it. Opening that today writes its
whole history out again as new logs (`Logged::write_and_merge`, when `last` isn't usable), which brings the document back
everywhere.

A move also reaches another machine as ordinary file changes, in whatever order they arrive, so a database can briefly look
left behind only because the rest is still in transit, hence the wait below. A machine whose `.stignore` lacks the line
gets the same protection, since its databases arrive the same way. (`~/data/.stignore` on this machine gained
`/writing/**.kame` while this was being written. The other machines' files weren't checked.)

**Telling what happened.** A database in a folder with logs is *stranded* if it has a recorded sync state
(`Db::load_sync_state`), it's at the place it last synced (`Db::load_sync_place`), and the logs beside it, if there are any,
don't contain that revision. Those logs may be missing, or may belong to a different document after a path
is reused, as with deleting X and making a new X, or swapping two names. Without this, the old database's history would be
written into the new document's logs and merged with it (`sync_in`, with no usable `last`), bringing deleted words into an
unrelated document. A line that only fails its check counts as containing what it says (`Store::could_hold`). A database that has never synced has no state and is left alone, as is anything with no `.kame-logs` above
it.

**The place.** A sync records where it happened, beside the sync state: the name of the document's logging root's folder and its
path from there, with `/` between folders, such as `writing/essays/a.kame`. It's what separates the logs having left a database from a database having been copied here, and a sync state
alone can't. The case that found this was in the real notebook: `essays/Politics/CommandingHeights.kame`, a byte-identical copy
made by hand of `../../CommandingHeights.kame`, with the original's sync state and no logs of its own, which would have been
retired ten minutes after first launch, with a false notice that it had been moved or deleted on another machine. So a place
that's missing or differs is not stranded, whatever the logs say. Being wrong that way is what kame did before this work. Being
wrong the other way hides someone's document.

A database that isn't stranded for this reason does what any database does. `Logged` writes its history as new logs, and it
records its new place, so that the document has its own logs from then on. Nothing retires it, and it lists as an ordinary
document. It's relative to the logging root so that a database carried to a machine where the notebook is somewhere else is
still in its place, and it has the root's name in it so that a copy into another notebook isn't. A root named differently on another
machine, or renamed, is merely not stranded until its next sync, which is the safe side. An older kame ignores the row, as it reads `meta` only by key, so there's no migration. An older kame's Save As
copies the place and leaves it there, which only makes that copy not stranded.

A database from before places were recorded gets one from a pass. If it has a sync state, no place, and its revision in the logs
beside it, that shows it synced there, so `decide` lists it, and `apply` records the place on the main thread, with the same looks
again as any other step, never for the open document, and ignoring a failure. Having a place is what stops it being asked again. A
hand copy with no logs beside it isn't stamped, nor is a database whose revision isn't beside it, whose logs are then read on each pass.
Neither a stamp nor the place recorded on arrival is written into a database holding something from a newer version of kame, which
this build writes nothing to, so that one is merely not known to be stranded until it's updated. A stranded database holding
anything from a newer version is neither retired nor moved: what this build reads of it leaves that out, so it could match
its logs while the file holds more, and it's left for an updated kame.
A stamp changes the file's modified time once, which a machine lacking the `**.kame` ignore line would carry, once per document. Neither a
stamp nor the place recorded on arrival at a home opens a database with a `-wal` or `-shm` beside it, as opening it for writing would fold
in a `-wal` that may be another machine's, with nobody looking, and it may be open in another process. A stamp is just asked again on the
next pass, which keeps no recheck alive, as nothing is waiting on it. A database that arrived with one keeps the place it had, so a
second move before it's next opened isn't recognised, and it's merely not stranded. A read-only file isn't asked, and nothing is left beside it.

The place follows the moves kame makes itself. `carry_out` syncs a moved document afterwards. `apply` records the new place as
each database arrives at a home, by opening it for writing on the main thread and ignoring a failure, so that a second move
before it's next opened is still recognised. If that fails, or something is beside it (see above), it's merely not stranded. A retired file keeps what it had, and is
found by its revision, whatever its place. Under a name it never synced at, it's a document of its own, which is how to get one back
by hand. Under the name it left it's stale again. Under its retired name it's not a document: opened there, it's held off as
stranded, whatever its place, since logs for it would go to every machine and be taken there for a home.

**Logs that may be another document's.** A database that isn't stranded can still have another document's logs beside it, if a
copy is put at a path that has logs, or a database with no place was left behind when its path was reused, or a verdict can't be
reached since a corrupt line is all there is. Logs count as a database's own only if there's nothing there, they hold the
revision it remembers, or a line in them says it's of this document (`Store::can_be_for`, by `Document::id`). Each line holds its
document's id inside its revision, so a line in an older format still says, even with an id that fails its check, and a line
that doesn't parse says if its JSON can still be read. Only a line with no id to read says nothing. A document's own old lines
therefore heal, and another document's don't count. Logs that say nothing, or that can't be read, show nothing, and "can't be told" means doing less.

Otherwise `Logged` writes nothing, as its history could reach another document's logs on every machine. `Outcome::others_logs` says
so, and the app says the logs may be another document's, and that Save As gives this one logs of its own. A delete leaves
those logs, and a move is refused, as for a stranded database. A pass leaves it alone, and doesn't take a copy like this at a stranded
database's new home for the moved document having been opened there. A database that has never synced merges with what's there, as
documents created through a link on two machines rely on (see the limits), but until it has, a delete or a move doesn't take what's
there. Nor does one whose file can't be read. With no file at all, the logs are the document, and a delete takes them.

**Lines that can't be read.** There are three kinds, and they aren't alike.

- A line that parses and fails its check. `Revision::id()` hashes the re-serialized struct, so every line written before a field was
  added to a revision is one, and every log will be full of them at the next such field. It can't be used, but it says what it held:
  its `id` is what a database's state names. So it rules a home in or out as well as a checked line does (`Store::could_hold`), and stops
  nothing. A document last synced in that format isn't stranded, and heals on its first sync, by writing its history again.
- A line torn by a crash counts for nothing (`serde_json::Error::is_eof`). It can't hold a remembered revision, as the state is saved only
  after its line is flushed, and `append` starts a new line after a tear rather than joining on to it.
- What's left is real corruption, and a revision that's missing may be in it, so that's "can't be told", and not stranded, unless the
  revision is found elsewhere (`Store::corrupt_lines`).

A pass needs every log only to rule out a home, which matters only for retiring a database as having no home. A move, a retirement
because the document has already been opened at its home, and a return have found their home, so they go ahead whatever else can't be
read. A no-home retirement is vetoed by a corrupt line in logs that could be of its document (`Store::may_hide`: a line there says
it's of this document, or none says whose they are), and by any folder or log that can't be read or holds lines from a newer kame. When
that stops something that has waited, the person is told once, until there's something new to say: "1 document looks left behind, and
can't be settled because *relative*'s logs can't all be read" (`Steps::unsettled`). A retirement whose revision is held only in a line that
fails its check is reported as `unsynced`, as what it holds can't be compared.

The two places that write a sync state (`Logged::write_and_merge` and `build_from_logs`) both write a revision from the
logs at that path, so a database in its right place always passes. The state is saved only once the log it names has been
flushed to disk, as a power cut between the two would otherwise leave a healthy database naming a revision its logs lack, which
would look stranded. The state and the place are saved in one transaction (`Db::save_sync`), as a place without its state would read
as stranded. `Session::save_as` clears both in the copy.

**What's done with a stranded database,** by a new `kame_notebook` function, for everything but the open document and any
database another kame has open, which is left for a later pass. The lock on each one acted on is held while it's renamed or
its place written, and that goes for a stamp too. Its
*new home* is the one log folder elsewhere in the notebook containing its recorded revision.

- **It moved** if it has a new home with no database there. It's renamed to join those logs, keeping its undo history and
  snapshots, and anything unsynced merges in the usual way when it's next opened. Where a swap makes the place of one
  stranded database the home of another, they move through temporary names. The revision is a better test than the
  document's id, which Save As copies share.
- **It's retired** if its new home already has a database that isn't itself stranded, because the moved document has
  already been opened here. Its undo history stays in the retired file, and the notice says so (`Done::Retired`'s `opened_there`).
- **It's left alone** if more than one log folder contains the revision, which shouldn't happen. The next pass sees it again.
- **It was deleted** if no log folder contains the revision. It's retired too.

Retired means renamed to `.X.retired.kame`, numbered if that's taken. The notice for a retirement says where the file is, and
whether it holds words that never synced (`unsynced`) or is only a copy of what's in the logs. A deleted document can't be
compared with anything, so it counts as unsynced.

- **A retired database comes back** if logs containing its revision later appear at a path with no database, since a moved
  document's logs can arrive after the old ones have gone. It's renamed to join them, as above.

**Why that name.** The notebook spec says `.kame.retired`, but `**.kame` wouldn't ignore that, so Syncthing would carry
what the machine retired to every other machine. A leading `.` and a final `.kame` is ignored by every line `enable` has
ever written, and the browser and listing already skip it. Words in a retired database are kept and reachable by hand, and
`docs/keybindings.md` says where.

**When.** When a notebook's found (`notebook_tick`, where `looked_from` changes), when a listing comes back different, every
minute while any database is still waiting out the wait below, and otherwise every 10 minutes while a notebook is open, since
a log folder being deleted doesn't change the listing, and the clock on it would start late. So a database that's waited and
can't be settled costs one pass in 10 minutes, not one a minute. A pass reads on a background thread and renames on the main
thread. Reading every log is slow, so it can't be on the main thread. But `build_from_logs` and opening a document run on the
main thread, and a build's rename replaces whatever is at its path, so a rename from another thread landing there would be
lost, and one in the middle of an open would move a file from under its session. So the thread only decides (`decide`), and
the main thread does what was decided (`apply`) when the result arrives, with the open path as of then, looking at each step
again just before taking it. That happens each time a pass finishes, even when nothing has waited long enough, since a retired
database coming back needs no wait. Passes never overlap, since a temporary name seen by a second pass would be taken for
something else; a due pass waits for the next tick.

Logs are read only for a stranded database, and for one that has no place yet, since a pass runs with nobody looking, on a laptop. Each pass remembers which
databases it found healthy (`Healthy`, which goes to the pass's thread and comes back with it, as the times first seen do),
with what that rested on: the revision and the place the database remembers, and each log's name and size, from `stat` and no reading. If
those are unchanged on the next pass, it's still healthy. Only not being stranded is remembered, and not a verdict that couldn't
be reached, so a change can only cause another read. Within a pass, every log folder is read only if something has waited long
enough to be acted on, or a retired or temporary database has logs with no database at their path to come back to, and that
reading serves the whole pass. Just before a database is acted on, its own logs are read again (`is_still_stranded`).

Afterwards the listing refreshes, and a notice says what happened to each of the first three, retirements first, with where the file is and what it holds, and
counting any more.

**The wait.** Nothing is moved or retired the first time it's seen stranded. The app remembers when it first saw each one,
in memory, and acts only if it's still stranded on a pass at least 10 minutes later. The notebook spec has a sentence
arriving in about 25 seconds and a change of machine taking minutes, so this outlasts a sync in progress. A session too
short to reach it simply leaves the database for the next one, and meanwhile the open-document rule below stops it writing
logs. The function itself takes the list of databases it may act on, so that it's testable without a clock.

**The open document.** If its logs vanish or are replaced while it's open, or it's opened stranded before a pass gets to it,
nothing may write logs for it, or it comes back. `Logged` holds off when the session is stranded by the test above, and the
app says once: "this document was moved or deleted on another machine, and isn't syncing." It stays editable, and
is dealt with by the pass after it's left. Its edits are then in the database that's moved or retired, so they aren't lost.

**Accepted limits:**

- A machine that wrote logs offline and reconnects after a delete can leave a partial log folder at the old path, and the
  document may come back from it. Nothing here tries to tell that apart.
- The same goes for a move. If B edits X offline, A renames X to Y and B reconnects, B's `.X.kame.sync/B.log` survives the sync
  tool's delete and reaches A as a logs-only X with no usable head. B's X isn't stranded, since its own partial log holds its
  revision, so B's edits never reach Y, and X can come back on A as an empty document. It's divergence and not loss: the words
  are in B's database and B's log.
- A database last synced before places were recorded, whose logs left before a pass first saw it, or before a pass could read them,
  has none and can't get one, so it isn't recognised as stale until it's next synced. Opening one on another machine after a
  delete can bring the document back, as it did before this work. After a path is reused, its history stays out of the new document's
  logs (see above), and it just doesn't sync.
- A move made by hand, with its logs, simply carries on. Without them it's treated like a copy: it starts logs of its own, and the
  old ones stay as a document with only its logs.
- A copy put at the same path in another notebook whose root's folder has the same name, with no logs, can't be told from a database
  whose logs have left, and it's taken for stale.
- A move goes ahead whatever else can't be read, so an unreadable or corrupt folder can hide a second home, and a move can then reach a
  copy, such as one holding the same revision, rather than the document's real home. Nothing is deleted, and it's recoverable by hand.
- A hand copy of a document unedited since its first sync reproduces the original's revision when it writes its history, so its logs
  can count as a second home for a stale database of that document, which is then left alone. One more edit ends it.
- A database that has never synced merges its words into what's at its path on its first sync, even another document's logs. Documents
  created through a link on two machines rely on this. `src/main.rs`'s Save As doesn't check for logs at the target, so it can do the same.
- A database whose logs are its own merges with another document's revisions there as well, as when a document deleted and made
  again at its name on another machine meets the old one's log ([merging, section 6.4](../../spec/merging.md)). Both keep all their
  words, and the app says it merged with another document of this name.
- A Save As copy shares its original's document id, so a hand copy of one, put on the other's logs, isn't told from its own and
  merges with them.
- A corrupt line that could be a document's stays until it's removed by hand, as logs are only added to, and until then that document
  can't be retired as having no home. The notice says which logs.
- Deliberately deleting a document's `.sync` folder while keeping the marker now retires its database. The docs only ever
  told people to remove both.
- `kame-site` doesn't run the clean-up, so a build on a machine before the app has opened the notebook can still publish a
  stale document. What's on this machine is what's published (section 10).
- A second running copy of kame with an affected document open sees "File changed on disk", as with any outside change.
- A second running copy of kame isn't protected from the race above: its `build_from_logs` or opening can land across a rename
  made by this one, and nothing here can tell.

## Where it goes

Logic in libraries, per `AGENTS.md`:

- `kame-core`: a link-rewriting `Session` method, a way to say a moved target (`links.rs`), and clearing sync state in
  `save_as`.
- `kame-sync`: `Logged` holding off for a missing log folder.
- `kame-notebook`: a new module with new-path checks, the delete, the plan and the move, and the clean-up. Each takes
  `device` where it needs one, and has no knowledge of a window.
- `src/notebook.rs`: the context menu, dialogs, report and the open document's part.

No new dependency.

## Testing

Headless tests in `kame-notebook`, as is usual there, using `kame-test-dir`. They need to cover:

- every link form above surviving a rename and a move, shown by `follow` leading to the new path from each document
- links in a closed logged document reaching its logs, and in a logs-only one
- `.md` and newer-version documents listed, and left byte-identical
- a move across folders fixing the moved document's own links and images, with its `#Heading` links unchanged
- a refused path changing nothing, including one with a file or only logs already there
- a rename changing a title that matched the old name, in a closed logged document too, and leaving one that didn't
- a link that exists only in a closed document's logs being rewritten
- two simulated machines sharing a log folder: a stranded database joining moved logs with history intact, retired after a
  delete, retired when the moved document is already open there, left alone when ambiguous, brought back when logs arrive
  late, and a Save As copy never mistaken for it
- a path reused by a new document, and two names swapped, with no old words reaching the new logs
- a never-synced database, and one outside a marked folder, untouched
- a delete leaving nothing behind, and `sync` writing nothing for a stranded session

The app's part is then checked by running it against a copy of a notebook: right-click, each operation, and the open
document following. Real notebooks aren't used for this.

## Documentation

`docs/keybindings.md` (replacing "To move or delete a document… move or delete both"), the notebook spec's sections 8, 10
and 13, the Move item in `docs/followups.md`, and the app's help, if it lists the browser.

## Left to the builder

Names, the module's shape, the dialog's layout, how the report is drawn (`src/publish.rs` has a similar one), and the order
of the work, provided the clean-up lands no later than the move and delete it makes safe.
