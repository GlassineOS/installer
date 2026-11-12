# SPDX-License-Identifier: GPL-2.0-or-later
# make check     the tests, lints and the description checks (what CI runs)
# make targets   the core and the program for every target the installer runs on
# make docker-check / docker-targets   the same inside the toolchain image (Dockerfile)
# make golden    rewrite the golden files, the screens' check reports and the schema (review the diff)
# make web-dev   the browser build and its test page in web/dev (needs wasm-bindgen-cli
#                at the version Cargo.lock pins for wasm-bindgen)
# make web-check         the web screens in headless Chromium (CHROMIUM=path) and their vendored files
# make docker-web-check  the same inside the web test image (web/Dockerfile)
# make web-records       rewrite the check reports the screens read (web/fixtures/reports/)

CARGO ?= cargo
BIN = $(or $(CARGO_TARGET_DIR),target)/debug/glassine-install
IMAGE ?= glassine-installer-ci
WEB_IMAGE ?= glassine-installer-web
FACTS = $(wildcard core/tests/fixtures/facts/*.json)

.PHONY: check test lint descriptions targets golden web-dev docker-image web-check web-records web-image docker-web-check

check: lint test descriptions

test:
	$(CARGO) test --workspace

lint:
	$(CARGO) fmt --all --check
	$(CARGO) clippy --workspace --all-targets -- -D warnings

# The check reports of the screens (web/fixtures/reports) are the program's output for the facts
# of the core's tests; check exits 3 for a machine it doesn't support.
descriptions:
	$(CARGO) build -p glassine-install
	$(BIN) validate --lock devices/lock.toml
	$(BIN) schema | cmp - schema/device-description-2.schema.json
	@for f in $(FACTS); do \
		{ $(BIN) check --facts $$f --json || [ $$? = 3 ]; } | cmp - web/fixtures/reports/$$(basename $$f) \
			|| { echo "web/fixtures/reports/$$(basename $$f) is stale: make web-records" >&2; exit 1; }; \
	done

web-records:
	$(CARGO) build -p glassine-install
	@mkdir -p web/fixtures/reports
	@for f in $(FACTS); do $(BIN) check --facts $$f --json > web/fixtures/reports/$$(basename $$f) || [ $$? = 3 ]; done

targets:
	$(CARGO) build --release -p glassine-install
	$(CARGO) build --release -p glassine-install --target x86_64-pc-windows-gnu
	$(CARGO) build --release -p glassine-install --target aarch64-unknown-linux-musl
	$(CARGO) build --release -p glassine-installer --target wasm32-unknown-unknown
	$(CARGO) rustc --release -p glassine-installer-web --target wasm32-unknown-unknown --crate-type cdylib

WASM = $(or $(CARGO_TARGET_DIR),target)/wasm32-unknown-unknown/release/glassine_installer_web.wasm

web-dev:
	$(CARGO) rustc --release -p glassine-installer-web --target wasm32-unknown-unknown --crate-type cdylib
	wasm-bindgen --target web --out-dir web/dev/pkg $(WASM)

golden: web-records
	UPDATE_GOLDEN=1 $(CARGO) test --workspace

web-check:
	web/tools/vendor check
	cd web && npm ci --ignore-scripts --no-audit --no-fund
	node web/test/screens.js

docker-image:
	docker build -t $(IMAGE) .

web-image:
	docker build -t $(WEB_IMAGE) web

docker-web-check: web-image
	docker run --rm -u "$$(id -u):$$(id -g)" -e HOME=/tmp -e npm_config_cache=/tmp/.npm -e CHROMIUM=/usr/bin/chromium \
		-e WEB_NO_SANDBOX=1 -v "$$PWD":/w -w /w $(WEB_IMAGE) make web-check

docker-%: docker-image
	docker run --rm -u "$$(id -u):$$(id -g)" -e CARGO_HOME=/w/.cargo-home -v "$$PWD":/w -w /w $(IMAGE) make $*
