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

The fuzzing tests (`fuzz_*.rs` in `kame-core`, `kame-typeset` and `kame-sync`) damage the releases' samples and what the tests build: `.kame` files through SQL, revision log lines, documents to merge, and Markdown to import. `cargo test` runs 150 to 400 seeds of each. Each also has a long run, ignored by default, which takes how many seeds from `KAME_FUZZ_ITERATIONS` (10,000 if unset) and the first from `KAME_FUZZ_SEED` (the clock if unset). It's quiet about the panics it catches, as when undo refuses a step, and ends by counting them, a line for each place they were. A failure prints every panic its seed had, with where it was, then its seed, what failed and where, and how to run it alone, and so does one that aborts:

```sh
KAME_FUZZ_ITERATIONS=100000 cargo test --release fuzz_ -- --ignored --nocapture
```

Every release keeps samples of what it writes, which every later build's tests read: a document and a folder of revision logs, with what reading them gave, in `crates/kame-core/tests/samples/VERSION/`. At the release commit, once every version says the new one and before tagging, run `cargo run -p kame-sync --example write_samples` and commit the folder it writes, with the release's section in `CHANGELOG.md`, which becomes its notes on GitHub. It's never run in tests. Nothing in a folder already there is ever changed, not even to make a test pass, and the release workflow refuses a tag whose version has none, or whose tests fail.

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
- **Revision bytes never change**: a revision's id and a block's digest are taken over the JSON this build writes for them again, so a change to that JSON makes every line already in every log fail its check. A field added to `Revision`, `Metadata`, `Annotation`, `Block` or anything they hold is left out when it's at its serde default (`#[serde(default, skip_serializing_if = ...)]`, skipped exactly at that default), and listed in `LEFT_OUT` in `crates/kame-core/src/model/document.rs`, which a test checks, so that another program writing it at its default isn't taken for a newer version. The golden tests at the end of `crates/kame-sync/src/revlog.rs` pin today's bytes, and must pass unchanged.

## Typography

Headings = Crimson Pro; body = EB Garamond; code = IBM Plex Mono. Fonts are `include_bytes!`-embedded from `crates/kame-typeset/fonts/` (13 faces: CrimsonPro Regular/Bold/Italic/BoldItalic, EBGaramond Regular/Bold/Italic/BoldItalic/SemiBold, IBMPlexMono Regular/Bold/Italic/BoldItalic) — never system font paths. An installed copy of these families is taken out (`font_system`), and the system's other fonts are only for text these can't set. Body shapes with `Shaping::Advanced` + `Align::Justified`. Punctuation hangs outside the measure (`hang_line`), except in code and prose blocks, so paint and hit-test from `BlockLayout::runs`, not `buffer.layout_runs()`. Justified left-to-right paragraphs are set by kame itself (`layout/breaks.rs`), with any hyphens drawn as glyphs with `start == end` that stand for no text.

## Module map

A workspace, so that documents can be used without a window. `cargo build` and `cargo test` at the root cover every crate.

`crates/kame-core` — documents, with no GUI or text engine, which is what lets a command line tool use it:

- `model/` — `Document`/`Block`/`Annotation` types + `anchor` shifting (byte offsets), and `offsets`, which fits a damaged file's spans and anchors to their text, a heading's level and a list item's depth to what the model has, and gives a repeated block or annotation id one of its own, as `Db` and the revision logs read them
- `storage/` — SQLite `Db` (WAL, `PRAGMA user_version` migrations, snapshots; `Db::open` makes a file that isn't there, so whatever writes into a document it found earlier opens it with `open_existing`, or with `open_with_nothing_beside` when nobody's looking), and `view`, what this build can show of a document holding a kind, style or value only a newer kame knows, and what a tool that only reads such a document takes of it, for reading and never for writing back
- `edit/` — `Op` log + `Editor` (undo/redo, commands)
- `export/` — `to_markdown`, `to_html` (deterministic), and `html_body` with `Publish` hooks for a tool that knows where links and images end up
- `links.rs` — where a link between documents leads, and how a document keeps an image's path
- `lock.rs` — `DocumentLock`, which holds a document for the kame that has it open, in a folder of the caller's, so that no other kame writes it, and `Locks`, which whatever writes into documents it doesn't have open takes them through
- `references/` — a notebook's sources: the `Reference` type, key rules, finding `[key]` citations, IEEE formatting and the text of the References section, which exports take as input, and `form`, what the windows for a new reference and for editing one hold
- `import/` — `from_markdown` (Markdown to a new `Document`, with a report of what it simplified)
- `session.rs` — `Session`: an open document, its file, caret and selection, and how every edit reaches the disk; `Session::open` makes a document where there's none, and `open_existing` is for one found earlier, which may have gone since; `open_as` takes which of the two an open is, an `Opening`, from whoever asks for it
- `text.rs` — moving through text by character and word
- `tests/samples/` — what each release wrote, with what reading it gave, which `tests/written_by_releases.rs` and `kame-sync`'s `tests/logs_from_releases.rs` read

`crates/kame-typeset` — how a document looks, reading it and never changing it:

- `layout/` — cosmic-text block shaping, choosing justified lines, anchor→Y mapping, marginalia placement
- `render/` — swash glyph-atlas rasterization + egui texture upload
- `fonts/` — the embedded faces

`crates/kame-site` — the static site generator, a command line tool on `kame-core` and `kame-notebook`, which the app also runs as `kame site`. `config` reads `site.yaml`, `source` and `pages` read what publishes, `refs` works out where links and images lead, `plan` works out the whole site with nothing written, and `write` replaces `output/` atomically. A notebook's site ends each page with the references it cites and warns about keys that look lost, and a `site.yaml` site has no references. Either publishes what this build can read of a document from a newer kame, and warns of it. The starting theme is under `theme/`, embedded for `kame-site new`.

`crates/kame-sync` — merging one person's diverged documents, on `kame-core`. The app uses `merge` and `files` for conflict copies, and `logs` and `revlog` for revision logs in folders that turn them on. `examples/write_samples` writes a release's samples. See `docs/superpowers/specs/2026-11-03-sync-design.md`.

`crates/kame-notebook` — a notebook: a folder with a `notebook.yaml`, on `kame-core` and `kame-sync`. It finds one, lists what's in it (a `.yaml` file directly in the root's `references/` is a reference, not an entry, and is read and written there as `KEY.yaml`), reading a document from a newer kame as far as this build can, searches it, works out where links lead and when one can create a document, and copies images in from outside. `manage` checks where a document may go, moves or renames one and rewrites the links to it, and deletes one. `stranded` settles databases left behind when a document was moved or deleted on another machine. Nothing derived is stored. The app's side of it is `src/notebook.rs`, and `kame-site` builds one as a site in `notebook.rs`. See `docs/superpowers/specs/2026-11-03-notebook-design.md`.

`crates/kame-check` — spelling and some grammar, offline, on `kame-core`, finding mistakes and leaving them to the app to draw, and skipping citations as it skips bare URLs. See `docs/superpowers/specs/2026-11-03-spelling-grammar-design.md`.

`crates/kame-test-dir` — folders for tests, only ever a dev-dependency. Each test process gets one, removed as it exits, so tests put their files here and never directly in the temporary directory. `fuzz` is what the fuzzing tests share: a seeded generator, ways of damaging bytes and JSON, and the short and long runs.

`src/notebook.rs` — the app's jump list, search, browser and what links here.

`src/references/` — the app's side of references. `mod.rs` keeps up with the notebook's list and holds what the windows share, `page` is which keys are drawn as citations and the entry a click on one opens, `notice` the notice for a key with no reference, `cite` Cite and its search, `form` the new reference and edit windows, `list` the References… window with its delete and the rows asking before a rename, `rename` the rename itself, and `browser` the browser's section for them.

`src/interface.rs` — the interface's text size: a scale of egui's text styles and the sizes beside them, and `room()` for kame's own fixed interface sizes; the keys that tell it from the type size.

`src/publish.rs` — the app's Site menu, on `kame-site`.

`src/commands.rs` — the menu bar's commands, in the one list the menus and the command palette (Ctrl-P) both draw from, the Edit menu's, which run what their keys do, and the palette.

`src/theme.rs` — Omarchy's current theme, which View > Follow Omarchy theme puts on the page and the interface around it: its `colors.toml` read, the palette and egui's look made from it, and a switch taken up while kame runs, leaving exports as they are.

`src/touchpad.rs` — a touchpad on Linux, sped up for a fast swipe and coasting once the fingers lift, as macOS does, before egui sees the frame's input.

`src/external.rs` and `src/macos.rs` — requests from the OS to open a file or quit, and the macOS application events that bring them.

`.github/workflows/release.yml` and `packaging/aur/` — releasing: a `v*` tag, refused without its version's samples, its section in `CHANGELOG.md` (its release notes) or with a failing `cargo test` (run on macOS, where the app builds with nothing to install), builds the macOS app, creates the GitHub release, bumps the Homebrew tap (`kisom/homebrew-tap`: the `kame` cask for the app, the `kame-site` formula) and installs both from it on a clean Mac to check them; the PKGBUILD and `.SRCINFO` are the AUR package, whose version, like the tag's, must match every `Cargo.toml` and `packaging/macos/Info.plist`.

`src/main.rs` — the app: egui shell (`KameApp`, keybindings, distraction-free modes).

`src/perf.rs` — how long the page takes over a real document: typing, deleting, scrolling and opening, at its size and ten times over. Ignored by default; its comment says how to run it.

The layering is one-way: core knows nothing of typeset or the app. Keep logic out of the app where it can go in a library. Keybindings, and how everything is used, are documented in `docs/keybindings.md`. Open work is in `docs/followups.md`.

## Conventions

- Pinned dependencies; no new runtime dependency without a spec change.
- Linux + macOS only.
