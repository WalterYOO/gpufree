BIN     := gpufree
PREFIX  ?= $(HOME)/.local

.PHONY: build install test check fmt clippy clean

build:
	cargo build --release
	mkdir -p bin
	cp target/release/$(BIN) bin/$(BIN)

install: build
	install -d $(PREFIX)/bin
	install -m 0755 bin/$(BIN) $(PREFIX)/bin/$(BIN)

test:
	cargo test

check:
	cargo check

fmt:
	cargo fmt

clippy:
	cargo clippy --all-targets -- -D warnings

clean:
	cargo clean
	rm -rf bin
