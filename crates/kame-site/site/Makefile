# Where `make sync` sends the site, as rsync understands it: user@host:/path/ or a local path.
UPSTREAM =

# Add --no-drafts here if what you sync shouldn't carry drafts.
BUILD_FLAGS =
KAME_SITE = kame-site
PORT = 8000

.PHONY: build serve sync clean

build:
	$(KAME_SITE) build $(BUILD_FLAGS)

serve: build
	python3 -m http.server -d output $(PORT)

# Upstream ends up matching output/, so anything else there is deleted.
sync: build
	@test -n "$(UPSTREAM)" || { echo "Set UPSTREAM at the top of the Makefile." >&2; exit 1; }
	rsync -avz --delete output/ "$(UPSTREAM)"

clean:
	rm -rf output .output-building .output-old
