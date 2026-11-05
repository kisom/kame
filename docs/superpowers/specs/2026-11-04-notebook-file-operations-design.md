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
left logs, and the app says so: "deleted the stale copy of *title*; the logs at that path may be another document's, and were
left". Without that a person reads the row as the delete not having taken, and deletes again, which with no database to say
otherwise removes the logs, on every machine.

If it's the open document, the app first syncs and leaves it: it goes to the notebook's contents page (`Notebook::index`)
if that isn't the one being deleted, and otherwise to a new anonymous document. The window's other memory of the path goes
too: the back stack (`BackStack::forget`), the recent list, `last_document` and any `fresh` entry.

The root `Notebook.kame` can be deleted like any other document. Home then says the notebook has none, as it does now.

### Renaming and moving

1. **Refuse** if the open document is the one moving and has a failed save, or if it's read-only from a newer version.
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
a newer version of kame. A document that can't be read isn't known to link at all and isn't listed, as with what-links-here.

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
(`Db::load_sync_state`) and the logs beside it, if there are any, don't contain that revision (`Store::contains`). Those
logs may be missing, or may belong to a different document after a path is reused, as with deleting X and making a new X,
or swapping two names. Without this, the old database's history would be written into the new document's logs and merged
with it (`sync_in`, with no usable `last`), bringing deleted words into an unrelated document. A database that has never
synced has no state and is left alone, as is anything with no `.kame-logs` above it.

The two places that write a sync state (`Logged::write_and_merge` and `build_from_logs`) both write a revision from the
logs at that path, so a database in its right place always passes. The state is saved only once the log it names has been
flushed to disk, as a power cut between the two would otherwise leave a healthy database naming a revision its logs lack, which
would look stranded. For this to be reliable, `Session::save_as` must clear
the sync state in the copy, since a copy would otherwise carry one from somewhere else, and be taken for a stale copy of the
original. A copy made before that change still does (see the limits below).

**What's done with a stranded database,** by a new `kame_notebook` function, for everything but the open document. Its
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

Logs are read only for a stranded database, since a pass runs with nobody looking, on a laptop. Each pass remembers which
databases it found healthy (`Healthy`, which goes to the pass's thread and comes back with it, as the times first seen do),
with what that rested on: the revision the database remembers, and each log's name and size, from `stat` and no reading. If
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
- A copy made before `save_as` cleared the state, and not opened since, still carries the original's revision. It says "moved
  or deleted on another machine", and the pass finds that revision in the original's logs with a healthy original there, and
  retires it with `opened_there`. On disk it can't be told from a real stale copy. Nothing is lost, since its words are in
  `.Z.retired.kame`, and `docs/keybindings.md` says how to get it back: open that and use Save As, which gives the copy logs of
  its own. Renaming the file isn't enough, as it would still carry the revision and be retired again.
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
