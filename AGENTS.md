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

Headings = Crimson Pro; body = EB Garamond; code = IBM Plex Mono. Fonts are `include_bytes!`-embedded from `fonts/` (10 faces: CrimsonPro Regular/Bold/Italic, EBGaramond Regular/Bold/Italic/SemiBold, IBMPlexMono Regular/Bold/Italic) — never system font paths. Body shapes with `Shaping::Advanced` + `Align::Justified`.

## Module map

- `src/model/` — `Document`/`Block`/`Annotation` types + `anchor` shifting (byte offsets)
- `src/storage/` — SQLite `Db` (WAL, `PRAGMA user_version` migrations, snapshots)
- `src/edit/` — `Op` log + `Editor` (undo/redo, commands)
- `src/layout/` — cosmic-text block shaping, anchor→Y mapping, marginalia placement
- `src/render/` — swash glyph-atlas rasterization + egui texture upload
- `src/export/` — `to_markdown`, `to_html` (deterministic)
- `src/import/` — `from_markdown` (Markdown to a new `Document`, with a report of what it simplified)
- `src/main.rs` — egui shell (`Session`, `KameApp`, keybindings, distraction-free modes)

Only `main.rs` knows about both egui and the DB; the layering is one-way. Keybindings are documented in `docs/keybindings.md`.

## Conventions

- Pinned dependencies; no new runtime dependency without a spec change.
- Linux + macOS only.
