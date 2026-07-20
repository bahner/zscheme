MA_FILES  := $(wildcard *.ma)
MD_FILES  := $(wildcard *.md)
CID_FILES := $(MA_FILES:.ma=.cid)

PREFIX    ?= /usr/local
BINDIR    ?= $(PREFIX)/bin

.PHONY: all build check test release install lint fmt fmt-check publish cids clean

all: build

# ── Rust binary ───────────────────────────────────────────────────────────────

build:
	cargo build

check:
	cargo check

test: fmt-check
	cargo clippy --all-targets -- -W clippy::pedantic -D warnings
	cargo test

release:
	cargo build --release

install: release
	sudo install -m 755 target/release/zscheme $(BINDIR)/zscheme

# ── Lint ──────────────────────────────────────────────────────────────────────

lint:
	cargo clippy -- -D warnings
	cargo fmt --check
	mdl $(MD_FILES)

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all --check

# ── IPFS ──────────────────────────────────────────────────────────────────────

# Publish all .ma files to IPFS and write their CIDs to matching .cid files.
publish: $(CID_FILES)

%.cid: %.ma
	@echo "Publishing $<…"
	@ipfs add -q --cid-version 1 "$<" | tee "$@"
	@echo "  → $< : $$(cat $@)"

# Print all stored CIDs.
cids:
	@for f in $(CID_FILES); do \
		name=$$(basename $$f .cid); \
		if [ -f "$$f" ]; then \
			printf '%-30s %s\n' "$$name" "$$(cat $$f)"; \
		else \
			printf '%-30s (not published)\n' "$$name"; \
		fi; \
	done

clean:
	@echo "Nothing to clean (keep .cid files in repo)"
