MD_FILES  := $(wildcard *.md)

PREFIX    ?= /usr/local
BINDIR    ?= $(PREFIX)/bin

.PHONY: all build check test release install lint fmt fmt-check publish zscheme-cid cids clean

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

# ── IPFS libraries ────────────────────────────────────────────────────────────

publish:
	$(MAKE) -C lib publish

zscheme-cid:
	$(MAKE) -C lib zscheme-cid

cids:
	$(MAKE) -C lib cids

clean:
	$(MAKE) -C lib clean
