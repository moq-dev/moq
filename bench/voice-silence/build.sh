#!/usr/bin/env bash
# Builds the C references against libopus 1.6.1 from nixpkgs: harness (SILK's VAD and its
# downsampler from source, as the port runs them) and encoder (the real libopus encoder).
set -euo pipefail
cd "$(dirname "$0")"
src=$(nix build --no-link --print-out-paths nixpkgs#libopus.src)
if [[ -f "$src" ]]; then
	mkdir -p opus
	tar -xzf "$src" -C opus --strip-components=1
	src=$PWD/opus
fi
dev=$(nix build --no-link --print-out-paths nixpkgs#libopus.dev)
lib=$(nix build --no-link --print-out-paths nixpkgs#libopus.out)

files=()
for name in VAD ana_filt_bank_1 sigm_Q15 lin2log resampler_private_down_FIR resampler_private_AR2 \
	resampler_rom resampler resampler_private_up2_HQ resampler_private_IIR_FIR; do
	files+=("$src/silk/$name.c")
done
gcc -O2 -DOPUS_BUILD -DHAVE_LRINTF -DVAR_ARRAYS -I"$src/silk" -I"$src/celt" -I"$src/include" \
	-I"$src/silk/fixed" harness.c "${files[@]}" -o harness
gcc -O2 -I"$dev/include/opus" encoder.c -L"$lib/lib" -Wl,-rpath,"$lib/lib" -lopus -o encoder
