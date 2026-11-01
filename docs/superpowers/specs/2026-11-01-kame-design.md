# kame — design

Date: 2026-11-01
Status: draft — for review

## 1. Purpose

kame ("Kyle's Marginalia Editor") is a WYSIWYG editor for writing ideas and thinking — prose,
not code. It is deliberately distinct from kte, Kyle's code/config editor: kte is terminal-first,
plain-text, and command-model; kame is typography-first and rich-document.

## 2. Goals

1. Beautiful typography — Crimson Pro for headers, EB Garamond for body.
2. Distraction-free writing modes.
3. Marginalia — short annotations in the margin, anchored to a passage (the defining feature).
4. Notes — extended comments anchored to a highlighted selection.
5. Footnotes — numbered markers collected at the foot.
6. Export to markdown and HTML.
7. Persistent, shareable edit history and point-in-time snapshots.
8. Written in Rust.

## 3. Non-goals (v1)

- A library/notebook browser (single document; the model is shaped to grow into one later).
- Collaborative/multi-author editing (the history is *portable*, not concurrent).
- Windows support.
- Code editing / syntax highlighting (kte's domain).
- Hyphenation and optimal (Knuth-Plass) line breaking.
- Folding marginalia into markdown footnotes on export.

## 4. Key decisions

1. **WYSIWYG rich document; markdown/HTML are export-only.** Chosen over markdown-as-source:
   the document needs first-class annotation objects with stable anchors, which flat markdown text
   cannot represent without ad-hoc conventions.
2. **Annotations are one family of anchored objects.** Anchor = `block_id` + character offset
   (notes add an end offset for the highlighted range). The anchor is *logical*, so reflow never
   moves it; edits within a block shift the offsets of anchors after the edit point.
3. **Storage is SQLite, not JSON.** Chosen over a flat JSON file: undo/history must persist across
   sessions and be shareable, and snapshots at points in time are required. Accumulating and
   querying history is a database's job; JSON is a snapshot format and would force a hand-rolled
   operation log plus compaction.
4. **egui (immediate-mode) shell + cosmic-text/swash rendering.** Chosen over Tauri/webview: the
   immediate-mode preference, a single Rust binary, and no webview. cosmic-text shapes (ligatures,
   old-style figures, justification); swash rasterizes; a thin layout layer does anchoring and
   collision. Validated by spike (see `cosmic-text-swash-render` skill).
5. **The native `.kame` file is private storage and is gitignored.** The markdown/HTML export is
   the diffable, human-facing, git-tracked artifact. Export output must be deterministic.

## 5. Document model

A document is:

- `Document { id, title, blocks: [Block], annotations: [Annotation] }`

- `Block { id (uuid), kind, text (String), spans }`
  - `kind`: Paragraph | Heading(level) | ListItem(ordered)
  - `spans`: inline formatting ranges `{start, end, style}` over `text` (v1: bold, italic).

- `Annotation { id (uuid), kind, anchor, body }`
  - `kind`: Marginal | Note | Footnote
  - `anchor`: `{ block_id, start, end? }` — `end` only for Notes (a range); Marginal and Footnote
    are points (`start == end`).
  - `body`: annotation text (String + optional spans).

Offsets are **byte offsets** into `block.text` (Rust `str` indices) — the same unit cosmic-text reports
in `glyph.start`/`end`, so the layout layer needs no conversion. The editing layer converts to
character positions via `str::char_indices()` only where a user-facing caret needs it.
**Anchor invariant:** on any edit within a block, annotations anchored in that block shift their
`start`/`end` by the edit delta when the edit point precedes them. This is the single correctness
rule the editor must never violate.

## 6. Storage (SQLite)

The `.kame` file is a SQLite database (WAL mode, `PRAGMA user_version` for schema migration).
Tables:

- `documents` — current state: one row holding the serialized `Document` (JSON blob via serde).
- `ops` — append-only operation log. Each row is a self-describing, invertible operation
  (`{seq, timestamp, kind, payload}`): text insert/delete, span change, annotation add/remove,
  block create/split/merge. **Undo** = apply inverse; **redo** = replay.
- `snapshots` — point-in-time full states `{id, timestamp, label?, state}`. Created explicitly
  ("snapshot now") and as periodic auto-checkpoints (default: every 1000 ops).

Every edit commits in a transaction: append `op`, then update the `documents` blob. This makes the
history the shareable artifact — handing someone the `.kame` file hands them the full history and
all snapshots. "Compacted" sharing is the current-state export, not the native file.

## 7. Rendering

- **egui** owns chrome: window, toolbar, side panels, dialogs, distraction-free toggles, scroll.
- **cosmic-text** lays out each block (`Shaping::Advanced`, `Align::Justified` for body).
- **swash** rasterizes glyphs into an egui texture atlas (cached; invalidated on edit); drawn as
  textured quads. CPU-atlas is backend-agnostic and spike-proven; `glyphon` is a later, wgpu-only
  optimization for subpixel AA.
- Hit-testing for caret/selection uses cosmic-text `Buffer::hit()`.

## 8. Editing & undo

- Caret and selection are `(block, offset)` positions; selection may span blocks.
- Edits are commands (insert/delete/format/annotate), each producing a logged, invertible `op`.
- Undo/redo read from `ops` (not a session-only stack) — history survives close/reopen.
- Block editing: Enter splits a paragraph, Backspace merges, heading/list via keyboard or toolbar.

## 9. Layout — body, marginalia, notes, footnotes

- **Body**: justified; ligatures + old-style figures enabled via shaping.
- **Marginalia**: a side column to the right of the text measure; each note is placed at its anchor
  line's Y and, when anchors crowd, notes collision-stack downward. Smaller type, muted color.
- **Notes**: the anchored range is highlighted in the body (subtle tint); a marker sits in the
  margin; clicking expands the note in a popover.
- **Footnotes**: superscript marker in the body (a small raised run); collected at the foot under a
  rule; auto-numbered.

## 10. Export

- **Markdown**: blocks → `#`/`-`/paragraphs; inline → `*em*`/`**strong**`; footnotes → `[^n]` +
  `[^n]: body`. Marginalia and notes are dropped in v1. Deterministic output (stable ordering and
  numbering) so exported files diff meaningfully in git.
- **HTML**: full fidelity. Marginalia → `<aside>` pinned at the anchor; notes → highlighted
  `<mark>` + expandable `<details>`; footnotes → `<sup>` links + footnote list; CSS carries the
  fonts, justified body, ligatures, and `font-variant-numeric: oldstyle-nums`.

## 11. Distraction-free modes

Fullscreen; hide-chrome; hide-margin-column; paragraph focus (dim the rest); typewriter (caret
centered). All are egui UI-state toggles.

## 12. Crash safety

WAL mode makes each edit transaction durable; a crash mid-edit leaves the last committed state and
op log intact. The `ops` log doubles as crash recovery.

## 13. Testing

- **Unit**: anchor offset-shifting invariant; document serde round-trip; export determinism and
  correctness (markdown + HTML).
- **Integration**: SQLite undo/redo/snapshot/restore round-trips; kill-mid-write reopen.
- **Layout**: headless render-to-buffer geometry assertions (as in the spike).
- **Manual**: egui smoke test — type, annotate, snapshot, export.

## 14. Project layout & dependencies

```
kame/
  Cargo.toml
  src/
    main.rs      # egui app entry
    model/       # document, block, annotation, anchor
    storage/     # sqlite: documents, ops, snapshots
    layout/      # cosmic-text shaping, line/justification, anchor→y
    render/      # swash glyph atlas → egui
    edit/        # caret, selection, commands, undo
    export/      # markdown, html
  fonts/         # bundled Crimson Pro + EB Garamond
  tests/
```

Dependencies: eframe/egui, cosmic-text, swash, rusqlite (bundled), serde + serde_json, uuid.
Deferred: hypher (hyphenation), a markdown/HTML writer library if hand-rolling proves wanting.

## 15. Deferred / open items

- Hyphenation + optimal line breaking.
- Marginalia-as-footnote in markdown export.
- Library/notebook (multiple documents) and collaboration.
- Images/attachments in documents.

## 16. Acceptance criteria

1. Create/open/save a `.kame` document.
2. Type prose WYSIWYG: Crimson Pro headers, EB Garamond justified body, ligatures and old-style
   figures visible.
3. Add anchored marginalia, notes (highlight + comment), and footnotes; all survive editing/reflow.
4. Close and reopen: document identical, undo history intact.
5. Snapshot at a point; restore it.
6. Export to markdown (prose + footnotes) and HTML (full fidelity, fonts/typography).
7. Builds and runs on Linux (Arch) and macOS.
8. Crash mid-edit → reopen with no corruption.
