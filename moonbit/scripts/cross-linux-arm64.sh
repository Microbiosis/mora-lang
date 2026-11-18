#!/usr/bin/env bash
# Cross-compile the MoonBit mora trial to Linux ARM64 (aarch64, glibc).
# Copyright (c) 2026 Microbiosis. BSD-3-Clause.
#
# Host: Linux x86_64. Prerequisites (Debian/Ubuntu):
#   sudo apt-get install gcc-aarch64-linux-gnu binutils-aarch64-linux-gnu
# Optional smoke test:
#   sudo apt-get install qemu-user-static
#
# MOON_CC must be a compiler path. Moon runs `cc -dumpmachine`. A string
# such as `zig cc -target …` is not a valid MOON_CC. A package-level `cc`
# does not cross-compile the shared runtime.

set -euo pipefail
cd "$(dirname "$0")/.."

: "${MOON_CC:=aarch64-linux-gnu-gcc}"
: "${MOON_AR:=aarch64-linux-gnu-ar}"
export MOON_CC MOON_AR

command -v "$MOON_CC" >/dev/null
command -v "$MOON_AR" >/dev/null
command -v moon >/dev/null

echo "MOON_CC=$MOON_CC ($("$MOON_CC" -dumpmachine))"
echo "MOON_AR=$MOON_AR"

moon build --target native --release --target-dir _build/aarch64-linux-gnu

bin="_build/aarch64-linux-gnu/native/release/build/cmd/mora/mora.exe"
file "$bin"
readelf -h "$bin"
python3 - "$bin" <<'PY'
import struct, sys
data = open(sys.argv[1], "rb").read(20)
assert data[:4] == b"\x7fELF", "not an ELF file"
ei_data = data[5]
machine = struct.unpack_from("<H" if ei_data == 1 else ">H", data, 18)[0]
print(f"e_machine={machine}")
if machine != 183:
    raise SystemExit(f"expected ELF e_machine 183 (AArch64), got {machine}")
PY
echo "ARM64 binary: $bin"
