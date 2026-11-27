# The spike's toolchain: Rust with the native, Windows, arm64-musl and wasm targets,
# and the wasm-bindgen CLI matching Cargo.lock.
FROM rust:alpine
RUN apk add --no-cache musl-dev \
 && rustup target add wasm32-unknown-unknown x86_64-pc-windows-gnu aarch64-unknown-linux-musl \
 && cargo install --locked wasm-bindgen-cli --version 0.2.130 \
 && rm -rf /usr/local/cargo/registry
