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
- **Document properties** — title, author, dates and tags, published with each
  export.
- **Markdown import** — open a `.md` file as a new document; the status line
  says what was simplified.
- **Headings and lists** — `Ctrl-Shift-1/2/3` for H1/H2/H3, `Ctrl-Shift-3` for
  a list, `Ctrl-Shift-0` for body text.

## Building

Dependencies: a Rust toolchain (edition 2021) and a C compiler. On Linux you
also need `pkg-config` and the X11/Wayland/Vulkan development libraries — see
[`flake.nix`](flake.nix) for the exact set. NixOS gets all of these through
direnv/`nix develop` (the `.envrc` wires up the flake); macOS and Arch install
them natively.

```sh
make          # debug build
make test     # run the test suite
make install  # release build, install binary + a desktop shortcut
make clean    # remove build artifacts
```

Or with cargo directly:

```sh
cargo build
cargo test
```

On NixOS:

```sh
nix develop   # dev shell with cargo/clippy/rustfmt/rust-analyzer
nix build .#  # build the package
nix run .#    # launch the binary
```

On macOS, build an `.app` bundle:

```sh
make mac-app   # builds target/kame.app (ad-hoc signed, for local use)
```

`packaging/macos/build-app.sh` assembles the bundle — the release binary, an
`Info.plist`, and an `kame.icns` icon generated from `packaging/kame.png` —
and ad-hoc signs it. Launch it with `open kame.app` (a bare `target/release/kame`
binary runs, but has no menu bar and won't take keyboard focus). Shipping the
app to others needs a Developer ID certificate and `notarytool` notarization.

## Using

```sh
kame                    # an anonymous document; the first save asks for a file
kame <document.kame>    # open (or create) a document
kame <notes.md>         # import Markdown as a new, unsaved document
```

`make install` also drops a `kame.desktop` launcher (with an icon) into
`~/.local/share/applications` and registers `.kame` files, so kame can be started
from the desktop or by opening a document. The flake's package does the same. A
`.kame` file is a SQLite database, so on a desktop without its own file-type
handling, such as Hyprland, `xdg-open` also needs the `mimetype` command
(`perl-file-mimeinfo` on Arch) to recognise it rather than treating it as SQLite. Saving
(`Ctrl-S`) on a document with no file opens the system save-as browser; closing
a window with unsaved anonymous text asks before it is lost.

Key bindings are listed in [docs/keybindings.md](docs/keybindings.md).

## The file format

A `.kame` document is a SQLite database: the current document, an append-only
log of every edit (persistent, shareable undo), and point-in-time snapshots.
The schema is versioned with `PRAGMA user_version`.
