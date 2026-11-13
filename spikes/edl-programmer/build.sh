#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-or-later
# Build and test-sign the programmer spike. Output: out/ (git-ignored).
#   out/programmer-a.elf       variant A, unsigned: one LOAD segment
#   out/programmer-b.elf       variant B, unsigned: A + XBL_SEC-typed trampoline segment
#   out/programmer-a-v5.mbn    hash segment MBN v5: the first image to send (RESEARCH.md)
#   out/programmer-b-v5.mbn    the second, if A is refused or never runs
#   out/programmer-a-v3.mbn    MBN v3, only to compare if v5 is refused
#   out/SHA256SUMS, out/phdrs.txt, out/programmer-{a,b}.dis
# Usage: ./build.sh    (needs Docker; builds the pinned toolchain image once, then offline)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
image=glassine-edl-programmer-build
docker build -q -t "$image" "$here" >/dev/null
mkdir -p "$here/out"
docker run --rm --network none -u "$(id -u):$(id -g)" \
	-e CARGO_HOME=/tmp/cargo -e HOME=/tmp \
	-v "$here:/src" -w /src "$image" sh -eu -c '
		objdump=$(echo "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objdump)
		elf=aarch64-unknown-none-softfloat/release/programmer
		cargo build --release --locked --target-dir /tmp/target-a
		cargo build --release --locked --target-dir /tmp/target-b --features xbl-sec-trampoline
		cp /tmp/target-a/$elf out/programmer-a.elf
		cp /tmp/target-b/$elf out/programmer-b.elf
		for v in a b; do "$objdump" -d out/programmer-$v.elf > out/programmer-$v.dis; done
		sign() { python3 /opt/qtestsign/qtestsign.py prog -v "$1" -o "out/$3" "out/$2" >/dev/null; }
		sign 5 programmer-a.elf programmer-a-v5.mbn
		sign 5 programmer-b.elf programmer-b-v5.mbn
		sign 3 programmer-a.elf programmer-a-v3.mbn
		cd out
		set -- programmer-a.elf programmer-b.elf programmer-a-v5.mbn programmer-b-v5.mbn \
			programmer-a-v3.mbn
		{ for f; do
			echo "== $f"; readelf -h -l -W "$f" | sed -n "/Entry point/p;/Program Headers/,/^\$/p"
		  done; } > phdrs.txt
		sha256sum "$@" > SHA256SUMS
	'
cat "$here/out/SHA256SUMS"
