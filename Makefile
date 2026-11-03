# kame — Kyle's Marginalia Editor
#
# The Rust toolchain is provided by the environment: direnv + the flake on
# NixOS, a native install on macOS and Arch. `cargo` is therefore on PATH
# wherever this is run, so the recipes call it directly.

CARGO ?= cargo
BIN := kame
SITE_BIN := kame-site
PREFIX ?= $(HOME)/.local/bin
XDG_DATA_HOME ?= $(HOME)/.local/share

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
# kame-site opens no window, so it needs none of that.
install: ## Install kame, kame-site, the desktop shortcut and .kame file type
	@if command -v nix >/dev/null 2>&1 && [ -f flake.nix ]; then \
		nix build .#; \
		mkdir -p $(PREFIX); \
		install -m 755 result/bin/$(BIN) $(PREFIX)/$(BIN); \
	else \
		$(CARGO) build --release; \
		mkdir -p $(PREFIX); \
		install -m 755 target/release/$(BIN) $(PREFIX)/$(BIN); \
	fi
	$(CARGO) build --release -p $(SITE_BIN)
	@install -m 755 target/release/$(SITE_BIN) $(PREFIX)/$(SITE_BIN)
	@mkdir -p $(XDG_DATA_HOME)/applications $(XDG_DATA_HOME)/icons/hicolor/256x256/apps $(XDG_DATA_HOME)/mime/packages
	@sed 's|@PREFIX@|$(PREFIX)|g' packaging/kame.desktop > $(XDG_DATA_HOME)/applications/kame.desktop
	@install -m 644 packaging/kame.png $(XDG_DATA_HOME)/icons/hicolor/256x256/apps/kame.png
	@install -m 644 packaging/kame.xml $(XDG_DATA_HOME)/mime/packages/kame.xml
	@command -v update-mime-database >/dev/null 2>&1 && update-mime-database $(XDG_DATA_HOME)/mime >/dev/null 2>&1 || true
	@command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database $(XDG_DATA_HOME)/applications >/dev/null 2>&1 || true
	@printf 'installed $(PREFIX)/$(BIN), $(PREFIX)/$(SITE_BIN), desktop shortcut, icon, and .kame file type\n'

mac-app: ## Build a macOS .app bundle (run on a Mac)
	./packaging/macos/build-app.sh
