# Fuzzing moq-mux

Coverage-guided fuzzing of the media parsers an untrusted publisher reaches: the fMP4,
Matroska, MPEG-TS, and FLV importers (fMP4, HLS, SRT, and RTMP ingest), the Annex-B and
AV1 OBU splitters, the CMAF fragment decoder, and the hang and MSF catalog consumers
(catalog subscription).

```bash
cargo install --locked cargo-fuzz
just rs fuzz fmp4
```

The targets are `fmp4`, `mkv`, `ts`, `flv`, `cmaf`, `annexb`, and `catalog`. Extra
arguments pass through to libFuzzer, so `just rs fuzz fmp4 -- -max_total_time=300`
bounds a run.

The Nightly workflow runs every target for five minutes, resumes from the previous run's
minimized corpus, and uploads failure inputs as workflow artifacts.

## How it fits together

The target bodies are not in `fuzz_targets/`; they are in `moq-mux`'s hidden `fuzz`
module (`src/fuzz.rs`), and the files here are shims that also register its counting
allocator. `just check` replays the same bodies on the pinned stable toolchain, bound
included, so a failure found here becomes a regression test that CI runs without anyone
installing cargo-fuzz.

`just rs fuzz` regenerates `seeds/` from `moq_mux::fuzz::seeds()` before each run: the
crate's media fixtures, wrapped in each target's framing. libFuzzer writes what it
discovers to `corpus/<target>/`, which is kept across runs and gitignored along with
`seeds/` and `artifacts/`. `just rs fuzz --minimize <target>` shrinks the corpus once it
has grown.

Calling `cargo fuzz` directly skips `just rs fuzz`, which turns off the release
profile's fat LTO (it drops the sanitizer-coverage globals, so the link fails) and
symbol stripping. Prefix direct calls with
`CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_STRIP=none`.

## Input framing

- `fmp4`, `mkv`, `ts`, `flv`: a write size byte (0 for the whole input), then the
  container bytes, written in pieces of that size as ingest reads a socket.
- `annexb`: a selector byte whose remainder modulo 3 picks H.264, H.265, or AV1 and whose
  quotient is the write size, then the stream.
- `cmaf`: a big-endian `u16` init segment length, the init segment, then one moof+mdat
  frame.
- `catalog`: a selector byte whose low bit picks the hang (0) or MSF (1) catalog track,
  then that track's frames, separated by NUL.

## What the targets assert

Beyond "does not panic", `fuzz::run` counts the heap a target allocates on its thread
and fails an input whose total bytes or allocations exceed a fixed base plus a constant
per input byte. Every allocation counts, not just what stays live, so the bound catches
work that grows faster than the input, such as rebuffering or reparsing on each write,
as well as memory that does. The constants and what sets them are documented in
`src/fuzz.rs`.

## When something fails

libFuzzer writes the input to `artifacts/<target>/crash-<hash>` and prints the `cargo fuzz`
command to reproduce it (add the prefix above). Fix the bug, then commit the input:

```bash
cp rs/moq-mux/fuzz/artifacts/fmp4/crash-<hash> rs/moq-mux/fuzz/regressions/fmp4/
```

`fuzz::tests::regressions` replays everything under `regressions/<target>/` as part of
the normal test suite, so the case stays covered.
