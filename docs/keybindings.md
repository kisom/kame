# Keyboard

The editing map lives in `command_for_key` (`src/render/egui_render.rs`); the
view-mode map lives in `mode_for_key` (`src/main.rs`). `Ctrl` below means `Cmd`
on macOS (`Modifiers::command`).

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
