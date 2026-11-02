# Keyboard

The editing map lives in `command_for_key` (`src/render/egui_render.rs`); the
view-mode map lives in `mode_for_key` (`src/main.rs`), and the file/export map
in `file_command_for_key` (also `src/main.rs`). `Ctrl` below means `Cmd` on
macOS (`Modifiers::command`).

| Key | Action |
| --- | --- |
| text, Enter, Backspace, Delete | insert, new paragraph, delete back/forward |
| Ctrl-Z / Ctrl-Shift-Z / Ctrl-Y | undo / redo / redo |
| Arrows, Home, End | move the caret (within the current block in v1) |
| drag, shift-click | select |
| Escape | close the note popover |

## Distraction-free modes

Every mode is view-only UI state: never persisted, never an undo step, and
typing is unaffected while any of them is on. The active modes are named in the
status panel while the chrome is visible.

| Key | Mode | Effect |
| --- | --- | --- |
| F11 | fullscreen | toggles the window fullscreen |
| Ctrl-Shift-H | hide chrome | hides the top panel (toolbar, status and help) |
| Ctrl-Shift-M | hide margin | collapses the margin gutter; marginal notes and note markers are not drawn (their anchored ranges stay marked, footnotes stay at the foot) |
| Ctrl-Shift-P | paragraph focus | dims every block but the one holding the caret |
| Ctrl-Shift-T | typewriter | keeps the caret's line at the vertical centre of the window; the page re-centres when the caret moves and can still be scrolled while idle |

Held keys do not auto-repeat a toggle: one press is one flip.

## Files, exports and snapshots

The exports write beside the document you opened: `<stem>.md` and `<stem>.html`
in the same directory as the `.kame` file (in the working directory when the
path has no directory part). They read the document as it stands, so nothing
has to be saved first, and a failed write says so in the status panel without
making the document read-only.

A snapshot is a complete copy of the document stored in the `.kame` file,
outside the undo history. Restoring one is the only command here that changes
the text, and it is deliberately **not** an undo step:

- it asks for confirmation first, because the current text is discarded;
- the restored text starts a fresh history, so the snapshot itself is the undo
  point — take one before a risky revision if you may want to come back;
- the snapshots are kept, so restoring a newer one is how a restore is undone.

| Key | Action | Effect |
| --- | --- | --- |
| Ctrl-Shift-E | export Markdown | writes `<stem>.md` beside the `.kame` file |
| Ctrl-Shift-B | export HTML | writes `<stem>.html`, a standalone page |
| Ctrl-Shift-S | snapshot now | prompts for an optional label, then stores the document |
| Ctrl-Shift-R | restore snapshot | lists the snapshots (newest first); choose one, then confirm |

Both dialogs take Enter as their primary action and Escape as cancel. Nothing
auto-repeats: one press is one command, and a held Enter cannot step through
the restore confirmation.

The same four commands are the toolbar's `File:` row, whose buttons carry the
export path they would write in their tooltip.

## Build

`cargo` is not on the default PATH on this machine; build in a nix shell:

```sh
nix-shell -p cargo rustc gcc pkg-config libxkbcommon libx11 vulkan-loader wayland \
  --run 'cargo build'
```

Headless run (no display), with Xvfb and the software Vulkan driver:

```sh
Xvfb :99 -screen 0 1400x1000x24 &
DISPLAY=:99 VK_ICD_FILENAMES=/run/current-system/sw/share/vulkan/icd.d/lvp_icd.x86_64.json \
  ./target/debug/kame /tmp/doc.kame
```
