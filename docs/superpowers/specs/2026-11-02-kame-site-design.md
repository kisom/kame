# kame-site — design

Date: 2026-11-02
Status: draft — for review

## 1. Purpose

`kame-site` is a command line tool that builds a static website from `.kame` documents and a
few Markdown pages. It is the stopgap for publishing until the notebook exists: kame documents
are the longer essays, and Markdown covers pages like "about" and "colophon".

Markdown export loses marginalia, which is why publishing has to work from `.kame` files
directly. This tool is what makes them publishable at full fidelity.

Background notes are in `~/data/writing/MultiKame.kame`.

## 2. Goals

1. Publish `.kame` documents as HTML, with marginalia, footnotes, nested numbering and images.
2. Publish Markdown pages.
3. Theme the site, Pelican or Hugo style, with listings by date and by tag.
4. Keep published URLs stable: adding or renaming anything must not move what is published.
5. Never change a document. Publishing only ever reads.
6. Share drafts for early feedback, without listing them.

## 3. Non-goals (v1)

- A development server. `python3 -m http.server -d output` is enough.
- Deployment. The output is a directory.
- Incremental builds. The sites are small, so every build is a whole build.
- RSS or Atom feeds, sitemaps and search. Worth adding later, and nothing here prevents them.
- Heading anchors. See §7.
- Copying files that ordinary links point at, such as a linked PDF. Put those in `static/`.
- Deploying under a sub-path. A site lives at the root of its domain.
- Source control for `.kame` files. Managing them is the author's concern, outside this tool.

## 4. Where it lives

A new crate, `crates/kame-site`, in this workspace, built as the binary `kame-site`. It depends
on `kame-core` and not on `kame-typeset`, so it needs no GUI or text engine.

The library will change while the two are built together, which is the reason to share a
repository for now. Splitting it out later would be straightforward.

### Dependencies

These are new, apply only to `kame-site`, and don't reach the editor:

| Crate | For | Why this one |
|---|---|---|
| `yaml_serde` 0.10 | `site.yaml` | Maintained under the YAML organisation. `serde_yaml` and `serde_yml` are both deprecated. |
| `minijinja` 3 | Templates | Small, and Jinja syntax, the family Pelican themes use. Escapes HTML by default. |
| `pulldown-cmark` 0.13 with `html` | Markdown pages | Already a kame dependency. This adds its HTML output. |

All three were tried in a scratch project, including the behaviour §5 and §9 rely on.

## 5. Site layout and configuration

```
.
├── README.md
├── site.yaml
├── pages/
│   ├── index.md
│   ├── about.md
│   └── colophon.md
├── static/
│   ├── images/
│   ├── files/
│   ├── css/
│   └── js/
├── theme/
└── output/
```

`site.yaml`:

```yaml
site:
  name: Example
  url: https://example.com
  description: Things I have been thinking about.
  author: K. Isom
  publish:
    commanding-heights: ~/data/writing/CommandingHeights.kame
    code-review: ~/data/writing/CodeReview.kame
    hexapod-writeup: ~/data/writing/hexapod/Writeup.kame
  drafts:
    some-conference-writeup: ~/data/writing/some_conference/Writeup.kame
```

- `publish` maps a **slug** to a file. The slug is written by hand and is the URL, so nothing
  about the file, its title or any other document can change it.
- `drafts` has the same form. See §8.
- Paths follow `kame_core::links`: `~` expands, and a relative path starts from the site's
  directory.
- Slugs are lowercase letters, digits and hyphens, and a slug appears once across `publish` and
  `drafts`. The same file can't be listed twice.
- `name` and `publish` are required, and an unknown key is an error, so a typo like `publsh`
  can't silently publish nothing.

**A duplicate slug must be an error.** Parsed into a plain map, YAML's last duplicate silently
wins, which was confirmed in the scratch project and would quietly drop a document. The loader
therefore reads these two maps through its own deserializer, which rejects a repeated key.

## 6. What gets built

| Source | URL |
|---|---|
| `publish` entry | `/k/<slug>/` |
| `drafts` entry | `/drafts/<slug>/` |
| Images in a document | Beside its page, as `/k/<slug>/<file>` |
| `pages/index.md` | `/` |
| `pages/about.md` | `/about/` |
| `pages/a/b.md` | `/a/b/` |
| Each tag | `/tags/<tag>/` |
| `static/**` | The same path from the site root |
| The theme's `static/**` | The same path from the site root |

Every page is an `index.html` in its own directory. Two sources wanting the same output path is
an error, whether a page and `/k/` or a page and a static file. `static/` overrides the theme's
own static files on purpose, since that is how a site adjusts a theme.

### Documents

Each is read with `Db::open_read_only` and rendered with `html_body`.

- **Title:** the metadata title, then the document's own, then the file name.
- **Date:** `published`, or `created` if that is empty. Documents with neither sort last.
- **Notes:** never publish, as in every export. Marginalia and footnotes do.
- **Repeated title:** a first heading repeating the title is left out of the body, as the
  Markdown export already does, because the theme prints the title. This needs moving somewhere
  `html_body` can share it.
- A file too old to read without migrating is an error telling you to open it in kame once, since
  a read-only open can't migrate it.

### Images

Published beside the page and linked relatively, as Hugo does. An image block's path resolves:

1. `http://` or `https://`: left alone.
2. An existing file, absolute or relative to the document: copied beside the page and linked by
   its file name. Two different files with one name become `a.png` and `a-2.png`, in reading order.
3. A path starting with `/` that exists under `static/`: left alone, which keeps site-root paths
   like `/files/images/a.png` working.
4. Otherwise a warning, with the path left as written. It is a broken image, not a broken build.

### Markdown pages

Rendered with `pulldown-cmark` directly, with tables, footnotes and smart punctuation. They don't
go through `from_markdown`, which simplifies what kame's model can't hold: a page should render
everything Markdown can.

Front matter sets `title`, and optionally `template`. Reusing it means making
`split_front_matter` public in `kame-core`. A page with no title takes its first heading, then
its file name.

## 7. Links between documents

For each document link, the `Publish` hook takes the target's path with
`split_document_target`, finds the file with `find_document` from the linking document's location,
and compares it, canonicalized, to the site's documents:

| Target | Published as |
|---|---|
| A published document | A link to `/k/<slug>/` |
| A draft, linked from a draft | A link to `/drafts/<slug>/` |
| A draft, linked from a published document | Its text, so publishing can't reveal a draft |
| Anything else, including a rotted link | Its text, as kame's own HTML export does |

A `#Heading` is ignored. A link to `file#Heading` goes to the top of that page, and a same-document
`#Heading` publishes as its text. Keeping these right is the author's job, as with any web link,
and heading ids can be added later.

Each link published as text is noted in the build output, so that rot is visible without failing the
build.

## 8. Drafts

A way to send someone an unfinished essay.

- Built to `/drafts/<slug>/`, with `<meta name="robots" content="noindex">`.
- Left out of the index and of tag pages.
- `kame-site build --no-drafts` leaves them out altogether, for a public deploy that shouldn't
  carry them.

A draft isn't private. Anyone with the URL can read it, and it is only as hidden as its URL.

## 9. Themes

A theme is a directory of templates and static files:

```
theme/
├── templates/
│   ├── base.html
│   ├── document.html
│   ├── page.html
│   ├── index.html
│   └── tag.html
└── static/
```

A default theme is embedded in the binary, and `theme/` overrides it template by template, so a site
with no theme works and adjusting one template doesn't mean copying the rest.

Variables available:

| Template | Variables |
|---|---|
| All | `site` (`name`, `url`, `description`, `author`), `kame_css` |
| `document.html` | `title`, `date`, `author`, `tags`, `body`, `url`, `draft` |
| `page.html` | `title`, `body`, `url` |
| `index.html` | `documents`, `body` (from `pages/index.md`, if any) |
| `tag.html` | `tag`, `documents` |

`documents` is every published document, newest first, then by slug. Templates escape HTML by
default, and `body` is passed as safe.

`kame_css` is `export::STYLE`, so a theme that uses it gets the marginalia layout, which floats in
wide windows and folds in narrow ones, with no work of its own.

### Fonts

The default theme carries EB Garamond, Crimson Pro and IBM Plex Mono with `@font-face`, since
`STYLE` names them and readers won't have them installed. All are OFL, so this is allowed, and the
licences go with them. `kame-site` embeds them from `crates/kame-typeset/fonts/` by path, with no
dependency on the crate.

## 10. Command line

```
kame-site build [--no-drafts] [DIR]
```

`DIR` defaults to the current directory, and it is the only command in v1.

- **Output:** built into a temporary directory beside `output/`, which replaces `output/` only once
  everything has succeeded. A failed build leaves the last good site alone, and nothing stale
  survives a successful one.
- **Errors:** all reported together rather than one at a time, and no output is replaced, with a
  non-zero exit.
- **Warnings:** missing images and links published as text are listed at the end, and don't fail
  the build.

### Deterministic output

The same input gives byte-identical output, which keeps kame's export rule and makes the output
diffable. There is no build timestamp, everything is sorted, and the output's file times aren't
relied on.

## 11. Changes needed in `kame-core`

Most of this landed already: `Db::open_read_only`, `html_body`, `Publish`, `links`. Remaining:

1. Share the repeated-title rule with `html_body`.
2. Make `split_front_matter`, and what it returns, public.

## 12. Testing

The crate separates working out the site from writing it:

- **Unit tests:** config errors, including duplicate slugs, bad slugs and unknown keys. Link
  resolution for each row of §7. Image resolution for each case in §6. Output path collisions.
- **End to end:** a small site in a temporary directory with real `.kame` files, checking the output
  tree and key markup. Built twice to confirm byte-identical output, and a failed build must leave
  the previous `output/` in place.
- **Read-only:** each source document is byte-for-byte unchanged after a build.
- **By eye:** build a site from real writing, including `CommandingHeights.kame`, and look at it in a
  browser, with marginalia at both wide and narrow widths.

## 13. Acceptance criteria

1. `kame-site build` turns a site directory with no `theme/` into a browsable site.
2. A published document shows its marginalia, footnotes, images and nested numbering.
3. Adding a document, or one that shares a file name with another, changes no existing URL.
4. A duplicate slug, missing document or output collision fails the build, naming each problem,
   and leaves the previous output in place.
5. A link between published documents works, and any other document link is plain text.
6. A draft is reachable at its URL, absent from the index and tags, and absent altogether with
   `--no-drafts`.
7. Building twice gives identical output, and no source document changes.
8. `cargo build` and `cargo test` at the root pass, and `kame-core` still has no GUI dependency.

## 14. Open questions

1. **Slugs for tags.** Proposed: lowercase, with spaces and punctuation becoming hyphens, and two
   tags that reduce to one slug being an error.
2. **Date format.** `published` is a free string today. Proposed: show it as written, and sort as
   text, which works for `YYYY-MM-DD` as kame writes it.
