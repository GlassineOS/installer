# SPDX-License-Identifier: GPL-2.0-or-later
# The installer's toolchain, for CI and local builds: Rust with the Linux musl,
# Windows, arm64 musl and WebAssembly targets, clippy and rustfmt, and wasm-bindgen-cli for the
# browser build (make web-pkg), at the version Cargo.lock pins for wasm-bindgen: the two must match.
FROM rust:1.99-alpine3.24
ARG WASM_BINDGEN=0.2.130
RUN apk add --no-cache musl-dev make mingw-w64-gcc \
 && rustup target add wasm32-unknown-unknown x86_64-pc-windows-gnu aarch64-unknown-linux-musl \
 && rustup component add clippy rustfmt \
 && cargo install --locked wasm-bindgen-cli --version "$WASM_BINDGEN" \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git
