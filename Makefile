CARGO ?= cargo
PREFIX ?= /usr/local
DESTDIR ?=
CODENAME ?= nyiotxa
CARGO_BUILD_FLAGS ?= --release --locked

nyi: $(wildcard src/*.rs) Cargo.toml Cargo.lock VERSION
	NYI_CODENAME='$(CODENAME)' $(CARGO) build $(CARGO_BUILD_FLAGS)
	cp -f target/release/nyi $@

install: nyi
	install -d $(DESTDIR)$(PREFIX)/bin
	install -m 755 nyi $(DESTDIR)$(PREFIX)/bin/nyi

check: nyi
	./nyi --version
	./nyi --no-info --frames 1 >/dev/null

ISH_TARGET ?= i586-unknown-linux-musl
ish:
	rustup target add $(ISH_TARGET)
	NYI_CODENAME='$(CODENAME)' $(CARGO) build $(CARGO_BUILD_FLAGS) --target $(ISH_TARGET)
	install -d dist
	install -m 755 target/$(ISH_TARGET)/release/nyi dist/nyi-ish

clean:
	$(CARGO) clean
	rm -f nyi
	rm -rf dist

.PHONY: install check ish clean
