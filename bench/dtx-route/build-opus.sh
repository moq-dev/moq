#!/usr/bin/env bash
# Build libopus to wasm32-wasi with zig. Usage: build.sh <out.wasm> <opt> [WITH_DECODER]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
src=$here/src/opus-1.6.1
out=$1; opt=$2; dec=${3:-}
cd "$src"
list() { make -s -f - print VAR="$1" <<MK
include silk_sources.mk
include celt_sources.mk
include opus_sources.mk
print:
	@echo \$(\$(VAR))
MK
}
files="$(list SILK_SOURCES) $(list SILK_SOURCES_FLOAT) $(list CELT_SOURCES) $(list OPUS_SOURCES) $(list OPUS_SOURCES_FLOAT)"
exports="enc_create enc_encode enc_in_dtx enc_lookahead enc_destroy mem_alloc mem_free"
defs=""
if [ -n "$dec" ]; then exports="$exports dec_create dec_decode"; defs="-DWITH_DECODER"; fi
eflags=""; for e in $exports; do eflags="$eflags -Wl,--export=$e"; done
zig cc -target wasm32-wasi -mexec-model=reactor $opt -msimd128 -DOPUS_BUILD -DUSE_ALLOCA -DHAVE_LRINTF -DNDEBUG $defs \
	-Iinclude -Isilk -Icelt -Isilk/float -I. \
	$files "$here/shim.c" -o "$out" -Wl,--gc-sections -Wl,--strip-all $eflags
