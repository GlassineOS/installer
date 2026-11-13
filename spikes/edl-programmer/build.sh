#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-or-later
# Build and test-sign the programmer spike. Output: out/ (git-ignored).
# Each variant NAME gives out/programmer-NAME.elf (unsigned), out/programmer-NAME-v5.mbn
# (test-signed, MBN v5, what is sent) and out/programmer-NAME.dis; see TESTPLAN.md for what
# each one is for. Also: out/programmer-a-v3.mbn, out/SHA256SUMS, out/phdrs.txt.
# Signing is stock qtestsign (pinned in the Dockerfile); it puts the hash segment right
# after the last segment, which the boot ROM accepts (device run 2026-11-13).
# Usage: ./build.sh    (needs Docker; builds the pinned toolchain image once, then offline)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
image=glassine-edl-programmer-build
docker build -q -t "$image" "$here" >/dev/null
mkdir -p "$here/out"
# name:cargo features (comma separated; empty = variant A)
variants='
a:
s:xbl-sec-trampoline
n:xbl-sec-trampoline,no-wdt
u:probe-unmask
r:probe-resets
g:probe-mask
e:probe-spin
d:probe-pshold
f:probe-ret
'
docker run --rm --network none -u "$(id -u):$(id -g)" \
	-e CARGO_HOME=/tmp/cargo -e HOME=/tmp -e VARIANTS="$variants" \
	-v "$here:/src" -w /src "$image" sh -eu -c '
		objdump=$(echo "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objdump)
		elf=aarch64-unknown-none-softfloat/release/programmer
		sign() { python3 /opt/qtestsign/qtestsign.py prog -v "$1" -o "out/$3" "out/$2" >/dev/null; }
		rm -f out/*.elf out/*.mbn out/*.dis
		names=
		for v in $VARIANTS; do
			n=${v%%:*} f=${v#*:}
			cargo build --release --locked --target-dir "/tmp/target-$n" \
				${f:+--features "$f"}
			cp "/tmp/target-$n/$elf" "out/programmer-$n.elf"
			"$objdump" -d "out/programmer-$n.elf" > "out/programmer-$n.dis"
			sign 5 "programmer-$n.elf" "programmer-$n-v5.mbn"
			names="$names programmer-$n.elf programmer-$n-v5.mbn"
		done
		sign 3 programmer-a.elf programmer-a-v3.mbn
		cd out
		set -- $names programmer-a-v3.mbn
		{ for f; do
			echo "== $f"; readelf -h -l -W "$f" | sed -n "/Entry point/p;/Program Headers/,/^\$/p"
		  done; } > phdrs.txt
		sha256sum "$@" > SHA256SUMS
	'
cat "$here/out/SHA256SUMS"
