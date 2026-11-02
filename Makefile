# kame — Kyle's Marginalia Editor
#
# The Rust toolchain is provided by the environment: direnv + the flake on
# NixOS, a native install on macOS and Arch. `cargo` is therefore on PATH
# wherever this is run, so the recipes call it directly.

CARGO ?= cargo
BIN := kame
PREFIX ?= $(HOME)/.local/bin

.PHONY: all build release test clean install

all: build

build: ## Compile (debug)
	$(CARGO) build

release: ## Compile (release)
	$(CARGO) build --release

test: ## Run the test suite
	$(CARGO) test

clean: ## Remove build artifacts
	$(CARGO) clean

install: release ## Build release and install into ~/.local/bin
	mkdir -p $(PREFIX)
	install -m 755 target/release/$(BIN) $(PREFIX)/$(BIN)
