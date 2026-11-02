# kame — Kyle's Marginalia Editor
#
# The Rust toolchain is provided by the environment: direnv + the flake on
# NixOS, a native install on macOS and Arch. `cargo` is therefore on PATH
# wherever this is run, so the recipes call it directly.

CARGO ?= cargo
BIN := kame
PREFIX ?= $(HOME)/.local/bin

.PHONY: all build release test clean install mac-app

all: build

build: ## Compile (debug)
	$(CARGO) build

release: ## Compile (release)
	$(CARGO) build --release

test: ## Run the test suite
	$(CARGO) test

clean: ## Remove build artifacts
	$(CARGO) clean

# On NixOS the X11/Wayland/Vulkan libs live in the Nix store and are dlopen'd
# at runtime, so a bare cargo build can't find them outside `nix develop`.
# `nix build .#` wraps the binary with the right LD_LIBRARY_PATH; install that.
# Elsewhere the libs are in standard paths and a plain release build suffices.
install: ## Install the binary and a desktop shortcut
	@if command -v nix >/dev/null 2>&1 && [ -f flake.nix ]; then \
		nix build .#; \
		mkdir -p $(PREFIX); \
		install -m 755 result/bin/$(BIN) $(PREFIX)/$(BIN); \
	else \
		$(CARGO) build --release; \
		mkdir -p $(PREFIX); \
		install -m 755 target/release/$(BIN) $(PREFIX)/$(BIN); \
	fi
	@mkdir -p $(HOME)/.local/share/applications $(HOME)/.local/share/icons/hicolor/256x256/apps
	@sed 's|@PREFIX@|$(PREFIX)|g' packaging/kame.desktop > $(HOME)/.local/share/applications/kame.desktop
	@install -m 644 packaging/kame.png $(HOME)/.local/share/icons/hicolor/256x256/apps/kame.png
	@printf 'installed $(PREFIX)/$(BIN), desktop shortcut, and icon\n'

mac-app: ## Build a macOS .app bundle (run on a Mac)
	./packaging/macos/build-app.sh
