# Run inside `nix develop`.
FIXTURES := target/parity-fixtures

.PHONY: all gst npm test test-rust test-js bench bench-native demo arch-src arch-docker clean

all: gst npm

## GStreamer plugin -> target/release/libgstsmkaf.so
gst:
	cargo build --locked --release -p gst-plugin-smkaf

## npm package -> npm/dist (also builds the wasm module)
npm:
	cd npm && npm ci && npm run build

test: test-rust test-js

test-rust:
	cargo test --locked --workspace

## native vs. wasm bit-exactness + worklet glue
test-js: npm
	cargo run --locked -q --release -p smkaf-tools -- fixtures $(FIXTURES)
	node tests/parity/parity.mjs $(FIXTURES) npm/dist/smkaf.wasm
	node tests/worklet/worklet.test.mjs npm/dist

## per-filter timings, native and wasm (same signal and parameter events)
bench: bench-native npm
	cargo run --locked -q --release -p smkaf-tools -- fixtures $(FIXTURES)
	node tests/bench/bench.mjs $(FIXTURES) npm/dist/smkaf.wasm

bench-native:
	cargo run --locked -q --release -p smkaf-tools -- bench

## browser playground on http://localhost:5173
demo: npm
	cd demo && npm install && npm run dev

## source tarball for packaging/arch/PKGBUILD
arch-src:
	packaging/arch/make-source.sh

## build, install and smoke-test the Arch package in a clean container
## -> packaging/arch/gst-plugin-smkaf-*.pkg.tar.zst
DOCKER ?= docker
arch-docker: arch-src
	$(DOCKER) run --rm -v $(CURDIR)/packaging/arch:/pkg \
		-e HOST_UID=$$(id -u) -e HOST_GID=$$(id -g) \
		archlinux:base-devel sh /pkg/docker-build.sh

clean:
	cargo clean
	rm -rf npm/dist demo/dist packaging/arch/*.tar.gz
