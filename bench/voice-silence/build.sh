#!/usr/bin/env bash
# Builds the C references against libopus 1.6.1 from nixpkgs: ref_analysis (src/analysis.c and the
# CELT FFT it runs on, from source) and dtx (the real libopus encoder).
set -euo pipefail
cd "$(dirname "$0")"
src=$(nix build --no-link --print-out-paths nixpkgs#libopus.src)
dev=$(nix build --no-link --print-out-paths nixpkgs#libopus.dev)
lib=$(nix build --no-link --print-out-paths nixpkgs#libopus.out)

files=("$src/src/analysis.c" "$src/src/mlp.c" "$src/src/mlp_data.c")
for name in kiss_fft modes mathops mdct celt entcode entenc entdec cwrs rate; do
	files+=("$src/celt/$name.c")
done
gcc -O2 -DOPUS_BUILD -DUSE_ALLOCA -DHAVE_LRINTF -I"$src/include" -I"$src/celt" -I"$src/silk" -I"$src/src" -I"$src" \
	ref_analysis.c "${files[@]}" -lm -o ref_analysis
gcc -O2 -I"$dev/include/opus" dtx.c -L"$lib/lib" -Wl,-rpath,"$lib/lib" -lopus -o dtx
