# SPDX-License-Identifier: GPL-2.0-or-later
# make check     the tests, lints and the description checks (what CI runs)
# make targets   the core and the program for every target the installer runs on
# make docker-check / docker-targets   the same inside the toolchain image (Dockerfile)
# make golden    rewrite the golden files and the schema (review the diff)

CARGO ?= cargo
BIN = $(or $(CARGO_TARGET_DIR),target)/debug/glassine-install
IMAGE ?= glassine-installer-ci

.PHONY: check test lint descriptions targets golden docker-image

check: lint test descriptions

test:
	$(CARGO) test --workspace

lint:
	$(CARGO) fmt --all --check
	$(CARGO) clippy --workspace --all-targets -- -D warnings

descriptions:
	$(CARGO) build -p glassine-install
	$(BIN) validate --lock devices/lock.toml
	$(BIN) schema | cmp - schema/device-description-2.schema.json

targets:
	$(CARGO) build --release -p glassine-install
	$(CARGO) build --release -p glassine-install --target x86_64-pc-windows-gnu
	$(CARGO) build --release -p glassine-install --target aarch64-unknown-linux-musl
	$(CARGO) build --release -p glassine-installer --target wasm32-unknown-unknown

golden:
	UPDATE_GOLDEN=1 $(CARGO) test --workspace

docker-image:
	docker build -t $(IMAGE) .

docker-%: docker-image
	docker run --rm -u "$$(id -u):$$(id -g)" -e CARGO_HOME=/w/.cargo-home -v "$$PWD":/w -w /w $(IMAGE) make $*
