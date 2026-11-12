# SPDX-License-Identifier: GPL-2.0-or-later
# The installer's toolchain, for CI and local builds: Rust with the Linux musl,
# Windows, arm64 musl and WebAssembly targets, clippy and rustfmt.
FROM rust:1.99-alpine3.24
RUN apk add --no-cache musl-dev make mingw-w64-gcc \
 && rustup target add wasm32-unknown-unknown x86_64-pc-windows-gnu aarch64-unknown-linux-musl \
 && rustup component add clippy rustfmt
