# Silence-suppression route harness

Measurement harness for [moq-dev/moq#7880](https://github.com/moq-dev/moq/pull/7880). It is committed once for a
permalink and removed in the next commit; nothing here lands on main.

1. `build-opus.sh` builds libopus 1.6.1 (`downloads.xiph.org`, sha256 `6ffcb593...`) to `wasm32-wasi` with zig,
   with `opus-shim.c`: `build-opus.sh enc.wasm -O2` for the page, `build-opus.sh full.wasm -O2 dec` for the analyzer.
   It expects the extracted tarball under `src/opus-1.6.1` beside the shim.
2. On macOS, make the speech: `say -o a.wav --file-format=WAVE --data-format=LEF32@48000 "Push to talk check. Six seven,
   the quick fox jumps over the lazy dog."` and `b.wav` from "Sounds fine here. Send the next clip when ready."
3. `python3 signal.py <dir with a.wav b.wav> <www>` writes `signal.f32` and `segments.json` (19 s, 48 kHz mono).
4. Put `index.html`, `harness.js`, `enc.wasm`, `signal.f32` in `<www>`. `run.sh <name> <port> <runs> -- <browser> "{url}"`
   serves it on 127.0.0.1 and waits for the page to POST `results/<name>.json` (set `DETACHED=1` for Safari's `open -a`
   and macOS Firefox, which re-exec).
5. `bun analyze.ts <www> full.wasm <wavdir> results/*.json` decodes every route with the same libopus, places packets
   on the timeline by their stamps, writes WAVs and prints `results.tsv`.
