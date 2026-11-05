# References

Keep a notebook's sources in one place, cite them by typing `[smith2020]`, and have exports end with a References section listing what was cited.

## Why

Kyle cites sources often enough to keep wanting this, has no good place for them today, and the same sources turn up across documents. Stopping to enter a source is fine, but leaving kame to do it isn't.

Kame is a place to write, so the page stays as it was typed. The written-out list belongs to what gets published, and a citation is only as heavy as the brackets around it.

## Goals

- Type `[key]` anywhere in the prose or a footnote to cite a source. The text stays exactly as typed.
- Enter and correct references inside kame, without stopping for anything that doesn't fit a form.
- One list per notebook, shared by its documents and synced with its files.
- HTML, Markdown and site output end with a References section holding only what that output cites, with entries in IEEE format.

## Non-goals

- Numbered citations. This was considered and dropped: readers see `[smith2020]`, not `[1]`. No numbering means no rules for numbering across footnotes, and no change to how citations are stored.
- Other citation styles, BibTeX or Zotero import, or lookup by DOI or URL. Kame stays offline, and there's nothing to import yet. An import could come later.
- Showing the References section on the page while writing.
- References outside a notebook, and on-page drawing or clicking of citations in footnotes and marginals. Both could come later.

## Anti-goals

These would make this a failure even if it worked:

- Rewriting what was typed. The document holds `[smith2020]` and nothing else changes it, apart from renaming a key when asked.
- A citation that silently vanishes from an export. An unresolved one stays in the output as typed.
- Anything reaching a site that no published document cites.
- Any cost to documents that never cite anything, including older kame versions being unable to edit them.

## How it works

### Citations

A citation is the text `[key]`, where `key` is a known reference. Nothing is stored apart from that text. The test is made wherever it's needed, the way `bare_link_ranges` finds URLs: it reads the block, stores nothing, and adding a reference after typing its key just works.

A match must have `[` and `]` around a whole key, with no `[` before it and no `(` after it, so `[[target]]` and `[text](url)` never match. Keys match case-sensitively.

**Where it counts:**

| Place | Citation? |
|---|---|
| Paragraphs, headings, list items, block quotes | Yes |
| Footnotes | Yes, in exports. They aren't drawn differently on the page (see below) |
| Marginals | Yes, in formats that carry marginals. Same on the page |
| Code blocks, prose blocks, inline code, a link's own words | No, as with other typed links |
| Notes | No. Notes never publish, so a citation in one mustn't put a source in a References section |
| Text imported from Markdown | Yes, by the same test |

Today HTML carries marginals and Markdown doesn't (`docs/guide.md`), so the same document can list one more source in HTML. Each format lists only what it carries.

### Keys

ASCII letters, digits, `-` and `_`, starting with a letter, at most 64 bytes. So `[1]` can never be a citation, and a key is always safe as a file name and in a URL fragment.

Keys that differ only in case aren't allowed, because the files that hold them would collide on macOS. If two such files arrive from another machine, the first in byte order is used and the other is reported.

A reference with the key `sic` would make every `[sic]` a citation. This is accepted, because known keys are drawn differently on the page and so it's easy to see.

### The list

A notebook keeps one file per reference, `references/KEY.yaml` under its root. The key is the file name and isn't repeated inside the file, so the two can't disagree.

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

A file per reference, rather than one `references.yaml`, is so that adding different sources on a laptop and a phone while they're apart doesn't produce a sync conflict, which would hide one side's additions from kame.

| `kind` | Fields |
|---|---|
| `article` | `authors`, `title`, `published_in`, `volume`, `number`, `pages`, `month`, `year`, `doi`, `url` |
| `conference` | `authors`, `title`, `published_in`, `location`, `month`, `year`, `pages`, `doi`, `url` |
| `book` | `authors`, `editors`, `chapter`, `title`, `edition`, `place`, `publisher`, `year`, `pages`, `url` |
| `web` | `authors`, `title`, `site`, `accessed`, `url` |
| `freeform` | `text` |

All are optional except `title` (or `text`). All are strings, or lists of strings for names. References have no field for private remarks, so nothing private can publish along with one.

As with `notebook.yaml`, an unknown field or kind is refused rather than ignored. A file that can't be read is reported by name, the others keep working, and kame doesn't overwrite it. Symbolic links under `references/` are skipped, as elsewhere in a notebook.

Writes go to a temporary file and are renamed into place. Reference edits aren't undo steps, the same as a document's metadata.

**Changes to the notebook's rules (`docs/spec/notebook.md`):**

- A `.yaml` file in `references/` in the root is a reference. It isn't listed as an entry and never publishes as a file.
- Anything else in that folder stays an ordinary entry, so an existing `references/` folder of documents loses nothing.
- Section 7.1 gains that a reference publishes only as an entry in the References section of a published document citing it.

Nothing derived is stored, as elsewhere in a notebook: how many documents cite a reference is worked out by reading them.

### Documents outside a notebook

They have no references in this version. `[key]` stays text, exports get no section, and Ctrl-Shift-C says it needs a notebook. A document moved out of its notebook goes the same way until it's back in one.

So **the document format doesn't change at all**. Older kame versions, the merge rules and the future Android version are unaffected by citing. A list inside a `.kame` file could be added later, but it would need rules for damaged entries, its own rename and delete, and a merge that doesn't bring deleted keys back.

### Entering a reference

Ctrl-Shift-C (Cite, also in the menus and the right-click menu), in a notebook. What it opens depends on the caret:

- **In or just after a `[key]` with no reference:** the new reference window, with that key filled in. Saving leaves the text alone, and it becomes a citation.
- **In or just after a known `[key]`:** that reference, for editing.
- **Anywhere else:** a search over keys, authors and titles, ignoring case. Choosing one inserts `[key]` at the caret, as an ordinary edit and one undo step. If nothing matches, the last row is a new reference.

Closing a `[key]` that has the shape of a key but no reference shows a passing note in the status row: "No reference called kleppmann2019. Ctrl-Shift-C adds it." It doesn't interrupt, so `[sic]` costs nothing.

**The new reference window has two tabs:**

- **Structured:** a kind, then its fields, with a live preview of the IEEE entry. Names are comma-separated and kept as typed, or semicolon-separated if there's a semicolon in the box, so that a name can hold a comma.
- **Freeform:** a key and a box of text, which goes into the References section exactly as typed. This is the catch-all for anything the form doesn't fit.

A key is suggested from the first author's last name and the year, adding `a`, `b` and so on if it's taken, and can be changed. Characters that can't be in a key are dropped. If that leaves nothing, the person types one.

**On the page**, a known key in a block is drawn in a link colour. Clicking it shows its IEEE entry with an Edit button. Spell checking skips it, as it does bare URLs.

**In footnotes and marginals**, this version has none of that. They're drawn as one plain string, and clicking one opens its edit box, so a `[key]` there is typed by hand and takes effect in exports. Ctrl-Shift-C and the status row's note don't apply in that box.

**A References… window** in a notebook lists every reference, with how many documents cite it, for browsing, adding, editing, renaming and deleting. A reference added from it isn't cited.

**The browser** has a References row below its folders, opening as a folder does and kept open or closed with them. Under it, a row for each key opens its edit form, its right-click menu asks to rename or delete in the References… window, and a New reference… row adds one without citing it. With another window up it says to close that first, as a form may hold typing. Reference files stay out of the tree itself.

- **Deleting** says how many documents cite it first. They then hold plain `[key]` text.
- **Renaming** rewrites `[old]` to `[new]` everywhere it counts as a citation in each document citing it: prose, footnotes and marginals, as an ordinary edit in that document. A note isn't touched. A document that can't be rewritten, or can't be read to see whether it cites the key, is named in the result, not skipped silently, and the old key is then kept so that `[old]` still resolves. Running the rename again once the problem is fixed finishes it. The old key's file is removed last, after the open document has followed, so a crash along the way leaves both.

### Exporting

HTML, Markdown and site output gain a `## References` section at the very end, after the footnotes. A document citing nothing gets no section, and its output is byte-identical to today's.

Entries are sorted by key, ignoring case, then by bytes. The output depends only on the references' contents, keeping export deterministic.

```text
[knuth1981] D. E. Knuth and M. F. Plass, "Breaking paragraphs into lines," Softw.: Pract. Exper., vol. 11, no. 11, pp. 1119–1184, Nov. 1981.
```

- **HTML and sites:** each citation links to `#ref-KEY`, in footnotes as well. Titles are in italics where IEEE italicizes them. Reference text is escaped. A `url` is a link unless `is_script_url` rejects it, and then it's text.
- **Markdown:** citations stay as text, escaped as now (`\[smith2020\]`). Each entry is a paragraph.
- **Sites:** the theme's stylesheet needs to style the section.

A section a person wrote by hand isn't looked for. They'd get two.

**A key with no reference.** This is where dropping numbered citations costs something: `[sic]` and a lost `[smith2020]` are the same text, so an export can't warn about every bracket. So it warns only about ones shaped like the suggested keys, letters of either case followed by four digits and an optional letter. Others stay as typed with no warning. Either way, the text is never dropped. Warnings use the site's existing report (`Problems`) and the app's export notice.

### IEEE format

This table and the examples below are the authority for this work. They come from the IEEE Reference Guide, in a copy dated 12 August 2022. IEEE has since moved the guide to a document that couldn't be read here, so a newer version may differ in small ways. If the current guide is readable when this is built, compare them and report any difference, but don't wait for it.

| Kind | Form |
|---|---|
| `article` | J. K. Author, "Name of paper," *Abbrev. Title of Periodical*, vol. x, no. x, pp. xxx–xxx, Abbrev. Month year, doi: xxx. |
| `conference` | J. K. Author, "Title of paper," in *Abbreviated Name of Conf.*, location, Month year, pp. xxx–xxx, doi: xxx. |
| `book` | J. K. Author, *Title of Book*, xth ed. City, Country: Publisher, year. With no edition, a period follows the title: *Title of Book.* City, … |
| `book` with `chapter` | J. K. Author, "Title of chapter," in *Title of Book*, X. Editor, Ed., xth ed. City, Country: Publisher, year, pp. xxx–xxx. |
| `web` | J. K. Author. "Page Title." Website Title. Accessed: Month day, year. [Online]. Available: URL |

- Two names are joined with `and`, and more with commas and `, and`.
- More than six names become the first name and *et al.*
- A DOI or accessed date goes before a URL, and a URL goes last.
- Every entry ends with a period unless it ends with a URL.
- A `url` on the other kinds adds `[Online]. Available: URL`.
- Empty fields are left out, with no stray punctuation.
- Kame doesn't abbreviate given names or journal titles. That's for the person typing them.

The guide's basic forms write `Abbrev. Month, year`, but its examples all write `Oct. 2011`, with no comma. This follows the examples. Its website section writes `[Online.]`, and everywhere else `[Online].`. This uses `[Online].`.

Examples from the guide, for the tests:

```text
B. Klaus and P. Horn, Robot Vision. Cambridge, MA, USA: MIT Press, 1986.
L. Stein, "Random patterns," in Computers and You, J. S. Brake, Ed. New York, NY, USA: Wiley, 1994, pp. 55–70.
D. Sarkar and K. V. Srivastava, "SRR-loaded antipodal Vivaldi antenna for UWB applications with tunable notch function," in Proc. Int. Symp. Electromagn. Theory, Hiroshima, Japan, 2013, pp. 466–469.
J. Zhao, G. Sun, G. H. Loh, and Y. Xie, "Energy-efficient GPU design with reconfigurable in-package graphics memory," in Proc. ACM/IEEE Int. Symp. Low Power Electron. Design (ISLPED), Jul. 2012, pp. 403–408, doi: 10.1145/2333660.2333752.
J. Smith and J. Doe. "Obama inaugurated as President." CNN.com. Accessed: Feb. 1, 2009. [Online]. Available: http://www.cnn.com/POLITICS/01/21/obama_inaugurated/index.html
```

The guide's quotation marks are curly, and kame's entries use them too.

### Where the code goes

The layering stays one-way, and there are no new dependencies (`kame-notebook` already has `yaml_serde`).

| Crate | What |
|---|---|
| `kame-core` | `references`: the type, key rules, finding citations, IEEE formatting and the section's text. Exports take the references to use, and `Publish` gains them for sites |
| `kame-typeset` | Draws known keys. Layout needs to be told which keys are known |
| `kame-check` | Skips known keys |
| `kame-notebook` | Reads and writes `references/`, skips it in listings, and counts citing documents |
| `kame-site` | For a notebook's site, passes references to `html_body` in pages and the feed, and reports unresolved keys. A `site.yaml` site gets none, and its `[key]` stays text |
| `src/` | The Cite search, the new reference window, the References… window, and the notice |

## Known weak points

- **A re-imported Markdown export:** its References section comes back as an ordinary heading and paragraphs. Exporting that again from a notebook that has those keys gives two sections. Left as it is, because the importer can't tell this apart from a section the person wrote.
- **Unresolved keys** are only warned about when they're shaped like a suggested key.
- **A mistyped key in a footnote** looks the same as a right one on the page, and shows up only at export.
- **Two machines changing the same reference while apart** still gives a sync conflict for that one file. The conflict copy is skipped by kame like any other.
- **Renaming a key** edits documents that aren't open. In a folder with revision logs, each is synced with its logs before and after, as a closed document is when a move rewrites its links, so a citation only another machine has written is renamed too. The old key's file can still reach another machine before the rewritten logs do, leaving `[old]` as plain text there for a while. A rename can't be run there as it stands, since the old key has no file. Wait for the rewritten documents to arrive, or add the old key again with the same contents and rename it, which the rule for running a rename again allows.
- **Renaming while a second kame has a citing document open:** that window keeps its old copy and can overwrite the rename. There's no per-document lock, and two programs writing one file isn't something kame is trying to support, so this doesn't get one.

## Testing

- IEEE entries against the guide's own examples, for each kind and each missing field.
- Citations: which places count, and `[[x]]`, `[x](y)`, `[1]`, inline code, multibyte text around a key, and offsets in bytes.
- Export: byte-identical output for a document citing nothing, the same output across runs, each format listing only what it carries, notes never counting, and escaping of hostile reference text.
- Format: citing changes nothing stored in a document beyond its text, and its digests are computed as before.
- Notebook: an existing `references/` folder of documents is still listed, a damaged or unknown-field file is reported and left alone, and a reference file never reaches `output/`.
- In the app, driven for real: type a citation with no reference, add it, see it recognized, then edit, rename and delete it. Export HTML and Markdown, and build a site.

## Left to the builder

- Menu placement and window layout, within the flow above.
- How known keys reach layout and the spell checker.
- Exactly how a citation nests with `<mark>` and other markup in HTML.
- Whether Ctrl-Shift-C stands. It looks free, but check it.

`docs/spec/notebook.md`, `docs/guide.md`, `docs/keybindings.md` and `AGENTS.md` change along with the code. `docs/spec/document-format.md` doesn't need to.
