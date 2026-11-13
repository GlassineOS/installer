# SPDX-License-Identifier: GPL-2.0-or-later
# Build toolchain for the programmer spike: Rust's bare-metal arm64 target (linked by the
# rust-lld Rust ships, no cross gcc) and qtestsign for the Qualcomm hash segment.
FROM rust:1.99.1-alpine3.24@sha256:44819b27be4c97b0e4d7203bd2e02c4820f97700211b6a37fb4c246a2de59383
# qtestsign (GPL-2.0-only, Stephan Gerhold): build-time tool, not shipped in the image.
ARG QTESTSIGN_COMMIT=f3df53a5f0e37ccee076541f30f9d5b8340fc2d3
RUN apk add --no-cache git python3 py3-cryptography binutils \
 && rustup target add aarch64-unknown-none-softfloat \
 && rustup component add llvm-tools \
 && git clone https://github.com/msm8916-mainline/qtestsign.git /opt/qtestsign \
 && git -C /opt/qtestsign checkout --detach "$QTESTSIGN_COMMIT"
