# AGENTS.md

## What this is

kame — "Kyle's Marginalia Editor" — a WYSIWYG prose editor for writing ideas and thinking, written in Rust. The user types into the typeset page; markdown and HTML are generated **only on export**, never the source of truth. The native file is a SQLite `.kame` database (append-only op log + point-in-time snapshots).

## Build & test

On macOS and Arch the toolchain is a native install, so `cargo` is on PATH. On NixOS (straylight) `cargo`/`rustc` are **NOT on PATH**, so prefix every cargo command:

```sh
nix-shell -p cargo rustc gcc pkg-config --run 'cargo build'
nix-shell -p cargo rustc gcc pkg-config --run 'cargo test'
```

or use the flake devshell (`nix develop`, or `direnv allow` with the bundled `.envrc`). Packaging: `nix build .#` / `nix run .#`.

Both `cargo build` and `cargo test` must pass (`make all` and `make test`). The test suite is headless (no display/GPU); fonts resolve via `env!("CARGO_MANIFEST_DIR")/fonts/`.

To run the app with no display on NixOS, use Xvfb and the software Vulkan driver:

```sh
nix-shell -p cargo rustc gcc pkg-config libxkbcommon libx11 vulkan-loader wayland --run 'cargo build'
Xvfb :99 -screen 0 1400x1000x24 &
DISPLAY=:99 VK_ICD_FILENAMES=/run/current-system/sw/share/vulkan/icd.d/lvp_icd.x86_64.json \
  ./target/debug/kame /tmp/doc.kame
```

## Non-negotiable invariants

- **All text offsets are byte offsets** (`str` indices), matching cosmic-text `glyph.start`/`glyph.end`. Never char indices; edited text may be multibyte.
- **Every text mutation goes through an edit command that produces an invertible `Op`** and maintains the anchor invariant: anchors in the edited block shift by the edit delta when the edit point precedes them. `DeleteText` must, before `shift_anchors`, remove annotations wholly inside the range and clamp partially-overlapping anchors. `InsertText` strictly inside a note's range widens its `end`.
- **Undo history is linear**: a new edit after undo truncates the redo tail. Snapshots are the no-loss net and are restored as an explicit user action (not an op).
- **Crash-safe persistence**: op + head + cursor + redo-tail truncation land in ONE transaction (`Db::record_edit`); snapshot restore is atomic (`Db::restore_state`).
- **Deterministic export**: `to_markdown`/`to_html` must be byte-identical across runs and round-trips — footnote/note numbering derives from reading order, never storage order or UUIDs.

## Typography

Headings = Crimson Pro; body = EB Garamond; code = IBM Plex Mono. Fonts are `include_bytes!`-embedded from `crates/kame-typeset/fonts/` (13 faces: CrimsonPro Regular/Bold/Italic/BoldItalic, EBGaramond Regular/Bold/Italic/BoldItalic/SemiBold, IBMPlexMono Regular/Bold/Italic/BoldItalic) — never system font paths. An installed copy of these families is taken out (`font_system`), and the system's other fonts are only for text these can't set. Body shapes with `Shaping::Advanced` + `Align::Justified`. Punctuation hangs outside the measure (`hang_line`), except in code and prose blocks, so paint and hit-test from `BlockLayout::runs`, not `buffer.layout_runs()`. Justified left-to-right paragraphs are set by kame itself (`layout/breaks.rs`), with any hyphens drawn as glyphs with `start == end` that stand for no text.

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

`crates/kame-site` — the static site generator, a command line tool on `kame-core` and `kame-notebook`, which the app also runs as `kame site`. `config` reads `site.yaml`, `source` and `pages` read what publishes, `refs` works out where links and images lead, `plan` works out the whole site with nothing written, and `write` replaces `output/` atomically. The starting theme is under `theme/`, embedded for `kame-site new`.

`crates/kame-sync` — merging one person's diverged documents, on `kame-core`. The app uses `merge` and `files` for conflict copies, and `logs` and `revlog` for revision logs in folders that turn them on. See `docs/superpowers/specs/2026-11-03-sync-design.md`.

`crates/kame-notebook` — a notebook: a folder with a `notebook.yaml`, on `kame-core` and `kame-sync`. It finds one, lists what's in it, searches it, and works out where links lead and when one can create a document. Nothing derived is stored. The app's side of it is `src/notebook.rs`, and `kame-site` builds one as a site in `notebook.rs`. See `docs/superpowers/specs/2026-11-03-notebook-design.md`.

`crates/kame-check` — spelling and some grammar, offline, on `kame-core`, finding mistakes and leaving them to the app to draw. See `docs/superpowers/specs/2026-11-03-spelling-grammar-design.md`.

`crates/kame-test-dir` — folders for tests, only ever a dev-dependency. Each test process gets one, removed as it exits, so tests put their files here and never directly in the temporary directory.

`src/notebook.rs` — the app's jump list, search, browser and what links here.

`src/external.rs` and `src/macos.rs` — requests from the OS to open a file or quit, and the macOS application events that bring them.

`src/main.rs` — the app: egui shell (`KameApp`, keybindings, distraction-free modes).

The layering is one-way: core knows nothing of typeset or the app. Keep logic out of the app where it can go in a library. Keybindings, and how everything is used, are documented in `docs/keybindings.md`. Open work is in `docs/followups.md`.

## Conventions

- Pinned dependencies; no new runtime dependency without a spec change.
- Linux + macOS only.
