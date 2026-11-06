# kame-site — design

Date: 2026-11-02
Status: built. Publishing a notebook came later, in `2026-11-03-notebook-design.md`.

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
3. Theme the site, Pelican or Hugo style, with a home page, an archive, and listings by tag.
4. Keep published URLs stable: adding or renaming anything must not move what is published.
5. Never change a document. Publishing only ever reads.
6. Share drafts for early feedback, without listing them.
7. Offer an RSS feed of the latest published documents.
8. Set up a working site with one command, with its templates there to edit.

## 3. Non-goals (v1)

- A development server. `python3 -m http.server -d output` is enough.
- Deployment. The output is a directory.
- Incremental builds. The sites are small, so every build is a whole build.
- Atom feeds, sitemaps and search. Worth adding later, and nothing here prevents them.
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
│   ├── about.md
│   └── colophon.md
├── static/
│   ├── images/
│   ├── files/
│   ├── css/
│   └── js/
├── theme/
│   ├── templates/
│   └── static/
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
- `name`, `url` and `publish` are required, and an unknown key is an error, so a typo like `publsh`
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
| The home page | `/` |
| The archive | `/archive/` |
| The list of tags | `/tags/` |
| `pages/about.md` | `/about/` |
| `pages/a/b.md` | `/a/b/` |
| Each tag | `/tags/<tag>/`, normalized: lowercase, with spaces and punctuation as hyphens. Two tags reducing to one are an error. |
| The RSS feed | `/feed.xml` |
| `static/**` | The same path from the site root |
| The theme's `static/**` | The same path from the site root |

Every page is an `index.html` in its own directory. Two sources wanting the same output path is
an error, whether a page and `/k/` or a page and a static file. `static/` overrides the theme's
own static files on purpose, since that is how a site adjusts a theme.

### Documents

Each is read with `Db::open_read_only` and rendered with `html_body`.

- **Title:** the metadata title, then the document's own, then the file name.
- **Date:** `published`, then `created`, then the file's ctime. See Dates below.
- **Notes:** never publish, as in every export. Marginalia and footnotes do.
- **Repeated title:** a first heading repeating the title is left out of the body, as the
  Markdown export already does, because the theme prints the title. This needs moving somewhere
  `html_body` can share it.
- A file too old to read without migrating is an error telling you to open it in kame once, since
  a read-only open can't migrate it.
- One holding something a newer kame wrote publishes what this build can read of it, with nothing
  in place of the rest, and the build warns: `"SLUG": it was written by a newer version of kame, so
  it may be missing parts; update kame to publish all of it`. Opening it in this kame changes
  nothing, so that isn't what to say.

### Dates

kame stores dates as free text today, with `YYYY-MM-DD` as a hint in the Properties dialog. RSS
needs real dates, and sorting should use them, so `kame-core` gains a small `Date` type with no new
dependency, building on the date arithmetic `iso_date` already has.

- **Accepted:** `YYYY-MM-DD`, and also `YYYY-MM-DD HH:MM` or `YYYY-MM-DD HH:MM:SS`, which is what
  Markdown imported from Pelican carries. The time is ignored.
- **Rejected:** anything else, including impossible dates like `2026-02-30`.
- **In a build:** a date that won't parse is an error naming the document, because a typo would
  otherwise quietly misplace it in the listings and the feed.
- **In kame:** the Properties dialog says so when a date won't parse, but saves it anyway. It is
  your text, and only publishing depends on it.

Every document therefore has a date, in the listings and the feed alike. Falling back to the file's
ctime gets a warning in the build, because it is the one date that isn't part of the document: it
moves with any change to the file, and a copy synced to another machine has its own. kame writes
`created` into every document it saves, so this should only happen with older files, and setting a
date in the document fixes it.

Themes get the date as `YYYY-MM-DD`.

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
- Left out of every listing and the feed.
- Its tags don't count: they get no page and no count, and the draft itself shows none.
- `kame-site build --no-drafts` leaves them out altogether, for a public deploy that shouldn't
  carry them.

A draft isn't private. Anyone with the URL can read it, and it is only as hidden as its URL.

## 9. Themes

A theme is a directory of templates and static files. The starting theme lives in the repository at
`crates/kame-site/theme/` and is embedded in the binary, with `include_str!` and `include_bytes!`,
so that `kame-site new` can write it out for you to edit.

Only `new` reads the embedded copy. A build uses `theme/` and nothing else, and a missing template is
an error, so what is on disk is exactly what publishes.

```
theme/
├── templates/
│   ├── base.html
│   ├── index.html
│   ├── archive.html
│   ├── tags.html
│   ├── tag.html
│   ├── document.html
│   └── page.html
└── static/
    ├── css/
    └── fonts/
```

| Template | Builds | Variables, beside `site` and `kame_css` |
|---|---|---|
| `index.html` | `/` | `documents` |
| `archive.html` | `/archive/` | `documents` |
| `tags.html` | `/tags/` | `tags` |
| `tag.html` | `/tags/<tag>/` | `tag`, `documents` |
| `document.html` | Each document | `title`, `date`, `author`, `tags`, `body`, `url`, `draft` |
| `page.html` | Each Markdown page | `title`, `body`, `url` |

- `site` has `name`, `url`, `description` and `author`.
- `documents` is the published documents, newest first and then by slug, each with `title`, `date`,
  `url` and `tags`. Drafts are never in it.
- `tags` is every tag on a published document, sorted by name, each with `name`, `url` and `count`.
- `kame_css` is `export::STYLE`, so a theme that uses it gets the marginalia layout, which floats in
  wide windows and folds in narrow ones, with no work of its own.

Templates escape HTML by default. `body` and every URL are passed as safe: `minijinja` escapes `/`, so
a URL would otherwise come out as `&#x2f;k&#x2f;slug&#x2f;`, which browsers read correctly but which
is ugly in the output.

### The home page

The home page is a template you edit, not generated content. The one `new` writes is along the lines
of this, with its paragraph there to replace and its list keeping to the latest five:

```html
{% extends "base.html" %}
{% block content %}
<p>A paragraph about this site.</p>

<h2>Latest</h2>
<ul>
{% for document in documents[:5] %}
  <li><a href="{{ document.url }}">{{ document.title }}</a> ({{ document.date }})</li>
{% endfor %}
</ul>
<p><a href="/archive/">All posts</a></p>
{% endblock %}
```

`/archive/` lists every document in the same form. `/tags/` lists the tags with their counts, and each
tag's page lists its documents. All of these are plain lists on one page, with no pagination and no
summaries, which suits a site of essays.

### Fonts

The theme `new` writes carries EB Garamond, Crimson Pro and IBM Plex Mono with `@font-face`, since
`STYLE` names them and readers won't have them installed. All are OFL, so this is allowed, and the
licences go with them. `kame-site` embeds them from `crates/kame-typeset/fonts/` by path, with no
dependency on the crate.

## 10. RSS

An RSS 2.0 feed at `/feed.xml`, with the 10 most recent documents from `publish`. Drafts and
Markdown pages are never in it.

- **Items:** the title, link, author, date and tags as categories, with the link as a permanent
  `guid`, so that a reader only sees an item as new once. A slug never changes, so neither does it.
- **Content:** the whole document, so that an essay can be read in a feed reader.
- **Absolute URLs:** feed readers don't reliably resolve relative links, so the body is rendered a
  second time for the feed, with `Publish` hooks giving `site.url`-based URLs for images and document
  links.
- **Marginalia:** a feed reader won't apply `kame_css`, so these appear inline where they are
  anchored. The text stays readable, if without the margin.
- **Deterministic:** no `lastBuildDate`, which would change on every build. `pubDate` on each item
  comes from the document, at midnight UTC.
- **XML:** written by hand with careful escaping, without a new dependency. It is a small, fixed
  format.

## 11. Command line

```
kame-site new DIR
kame-site build [--no-drafts] [DIR]
kame-site regen-theme [DIR]
```

### `kame-site new`

Like `pelican-quickstart`, this sets up a site that builds and browses straight away. It writes:

- `site.yaml`, with placeholders and commented examples of `publish` and `drafts`
- `pages/about.md`, as an example page
- `theme/`, with every template, the stylesheet and the fonts
- `static/`, with `images/`, `files/`, `css/` and `js/`
- `README.md`, saying how to build and preview it, and a `.gitignore` for `output/`
- a `Makefile` with `build`, `serve`, `clean` and `sync`, with `UPSTREAM` at its top for rsync

It asks no questions: the placeholders are in one short file you edit next. It refuses to run in a
directory that already has anything in it, so it can't overwrite a site. Once written, the theme is
yours.

### `kame-site regen-theme`

Puts the embedded theme back at `theme/`, for picking up a newer one. A theme that differs at all,
including an added file, is moved to `theme.old`, then `theme.old-2` and so on, and never overwritten.
A theme that is already current is left alone, with no copy. Only `theme/` changes, and a directory
with no `site.yaml` is refused.

### `kame-site build`

`DIR` defaults to the current directory.

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
relied on. The one input outside the documents is a ctime fallback, which is why that warns.

## 12. Changes needed in `kame-core`

Most of this landed already: `Db::open_read_only`, `html_body`, `Publish`, `links`. Remaining:

1. Share the repeated-title rule with `html_body`.
2. Make `split_front_matter`, and what it returns, public.
3. Add `Date`, with parsing and RFC 822 formatting for RSS.

## 13. Testing

The crate separates working out the site from writing it:

- **Unit tests:** config errors, including duplicate slugs, bad slugs and unknown keys. Link
  resolution for each row of §7. Image resolution for each case in §6. Output path collisions.
- **New:** `new` followed straight away by `build` gives a browsable site with no documents. `new` in
  a non-empty directory refuses and changes nothing.
- **End to end:** a small site in a temporary directory with real `.kame` files, checking the output
  tree and key markup. Built twice to confirm byte-identical output, and a failed build must leave
  the previous `output/` in place.
- **Dates:** each accepted form, impossible dates, and leap years, with weekdays checked for RFC
  822. The order of `published`, `created` and ctime, and the warning for ctime.
- **RSS:** the newest 10 only, no drafts or pages, escaping, and absolute URLs. The output is parsed
  back in the end-to-end test to confirm it is well-formed.
- **Read-only:** each source document is byte-for-byte unchanged after a build.
- **By eye:** build a site from real writing, including `CommandingHeights.kame`, and look at it in a
  browser, with marginalia at both wide and narrow widths.

## 14. Acceptance criteria

1. `kame-site new` then `kame-site build` gives a browsable site, with no other steps.
2. A published document shows its marginalia, footnotes, images and nested numbering.
3. Adding a document, or one that shares a file name with another, changes no existing URL.
4. A duplicate slug, missing document or output collision fails the build, naming each problem,
   and leaves the previous output in place.
5. The home page shows what its edited template says, with the latest documents, and `/archive/`,
   `/tags/` and each tag's page list the right documents.
6. A link between published documents works, and any other document link is plain text.
7. A draft is reachable at its URL, absent from every listing and the feed, and absent altogether
   with `--no-drafts`.
8. Building twice gives identical output, and no source document changes.
9. `/feed.xml` validates as RSS 2.0, holds the 10 newest published documents, and its links and
   images work from outside the site.
10. A date that won't parse fails the build, naming the document.
11. `cargo build` and `cargo test` at the root pass, and `kame-core` still has no GUI dependency.
