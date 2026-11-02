# kame

**Kyle's Marginalia Editor** — a WYSIWYG editor for writing ideas and thinking.

You type into the typeset page; kame renders your prose with real typography
(Crimson Pro headings, EB Garamond body) and lets you write alongside it in
the margin. Markdown and HTML are generated only on export — the document
itself is a structured, first-class file, never a pile of markup.

## Features

- **Anchored marginalia** — notes in the margin that stay with their paragraph.
- **Notes** — highlight a passage and attach an extended comment.
- **Footnotes** — numbered markers, collected at the foot of the page.
- **Persistent undo** — the edit history lives in the file, so it survives
  closing and reopening, and can be shared.
- **Snapshots** — save a point-in-time copy of the document and restore it
  later.
- **Distraction-free modes** — fullscreen, hide chrome, hide margin, paragraph
  focus, typewriter.
- **Export** — deterministic Markdown and full-fidelity HTML.
- **Headings and lists** — `Ctrl-Shift-1/2/3` for H1/H2/H3, `Ctrl-Shift-3` for
  a list, `Ctrl-Shift-0` for body text.

## Building

Rust (edition 2021), egui + cosmic-text + swash + SQLite.

```sh
# On NixOS (or any Nix host):
nix build .#            # builds and runs the test suite
nix run .#              # launch the binary
nix develop             # dev shell with cargo/clippy/rustfmt/rust-analyzer

# With a bare cargo toolchain:
cargo build
cargo test
```

## Using

```sh
kame <document.kame>    # open (or create) a document
```

Key bindings are listed in [docs/keybindings.md](docs/keybindings.md).

## The file format

A `.kame` document is a SQLite database: the current document, an append-only
log of every edit (persistent, shareable undo), and point-in-time snapshots.
The schema is versioned with `PRAGMA user_version`.
