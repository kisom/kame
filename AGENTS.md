# AGENTS.md

## What this is

kame — "Kyle's Marginalia Editor" — a WYSIWYG prose editor for writing ideas and thinking, written in Rust. The user types into the typeset page; markdown and HTML are generated **only on export**, never the source of truth. The native file is a SQLite `.kame` database (append-only op log + point-in-time snapshots).

## Build & test

`cargo`/`rustc` are **NOT on PATH** on the primary dev host (straylight, NixOS). Prefix every cargo command:

```sh
nix-shell -p cargo rustc gcc pkg-config --run 'cargo build'
nix-shell -p cargo rustc gcc pkg-config --run 'cargo test'
```

or use the flake devshell (`nix develop`, or `direnv allow` with the bundled `.envrc`). Packaging: `nix build .#` / `nix run .#`.

Both `cargo build` and `cargo test` must pass. The test suite is headless (no display/GPU); fonts resolve via `env!("CARGO_MANIFEST_DIR")/fonts/`.

## Non-negotiable invariants

- **All text offsets are byte offsets** (`str` indices), matching cosmic-text `glyph.start`/`glyph.end`. Never char indices; edited text may be multibyte.
- **Every text mutation goes through an edit command that produces an invertible `Op`** and maintains the anchor invariant: anchors in the edited block shift by the edit delta when the edit point precedes them. `DeleteText` must, before `shift_anchors`, remove annotations wholly inside the range and clamp partially-overlapping anchors. `InsertText` strictly inside a note's range widens its `end`.
- **Undo history is linear**: a new edit after undo truncates the redo tail. Snapshots are the no-loss net and are restored as an explicit user action (not an op).
- **Crash-safe persistence**: op + head + cursor + redo-tail truncation land in ONE transaction (`Db::record_edit`); snapshot restore is atomic (`Db::restore_state`).
- **Deterministic export**: `to_markdown`/`to_html` must be byte-identical across runs and round-trips — footnote/note numbering derives from reading order, never storage order or UUIDs.

## Typography

Headings = Crimson Pro; body = EB Garamond; code = IBM Plex Mono. Fonts are `include_bytes!`-embedded from `crates/kame-typeset/fonts/` (10 faces: CrimsonPro Regular/Bold/Italic, EBGaramond Regular/Bold/Italic/SemiBold, IBMPlexMono Regular/Bold/Italic) — never system font paths. Body shapes with `Shaping::Advanced` + `Align::Justified`. Punctuation hangs outside the measure (`hang_line`), except in code and prose blocks, so paint and hit-test from `BlockLayout::runs`, not `buffer.layout_runs()`. Justified left-to-right paragraphs are set by kame itself (`layout/breaks.rs`), with any hyphens drawn as glyphs with `start == end` that stand for no text.

## Module map

A workspace, so that documents can be used without a window. `cargo build` and `cargo test` at the root cover every crate.

`crates/kame-core` — documents, with no GUI or text engine, which is what lets a command line tool use it:

- `model/` — `Document`/`Block`/`Annotation` types + `anchor` shifting (byte offsets)
- `storage/` — SQLite `Db` (WAL, `PRAGMA user_version` migrations, snapshots)
- `edit/` — `Op` log + `Editor` (undo/redo, commands)
- `export/` — `to_markdown`, `to_html` (deterministic), and `html_body` with `Publish` hooks for a tool that knows where links and images end up
- `links.rs` — where a link between documents leads
- `import/` — `from_markdown` (Markdown to a new `Document`, with a report of what it simplified)
- `session.rs` — `Session`: an open document, its file, caret and selection, and how every edit reaches the disk
- `text.rs` — moving through text by character and word

`crates/kame-typeset` — how a document looks, reading it and never changing it:

- `layout/` — cosmic-text block shaping, choosing justified lines, anchor→Y mapping, marginalia placement
- `render/` — swash glyph-atlas rasterization + egui texture upload
- `fonts/` — the embedded faces

`crates/kame-site` — the static site generator, a command line tool on `kame-core`. `config` reads `site.yaml`, `source` and `pages` read what publishes, `refs` works out where links and images lead, `plan` works out the whole site with nothing written, and `write` replaces `output/` atomically. The starting theme is under `theme/`, embedded for `kame-site new`.

`crates/kame-sync` — merging one person's diverged documents, on `kame-core`. The app uses `merge` and `files` for conflict copies; `revlog` is still a proof of concept. See `docs/superpowers/specs/2026-11-03-sync-design.md`.

`crates/kame-check` — spelling and some grammar, offline, on `kame-core`, finding mistakes and leaving them to the app to draw. See `docs/superpowers/specs/2026-11-03-spelling-grammar-design.md`.

`src/main.rs` — the app: egui shell (`KameApp`, keybindings, distraction-free modes).

The layering is one-way: core knows nothing of typeset or the app. Keep logic out of the app where it can go in a library. Keybindings are documented in `docs/keybindings.md`.

## Conventions

- Pinned dependencies; no new runtime dependency without a spec change.
- Linux + macOS only.
