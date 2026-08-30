# Fuzzing moq-net

Coverage-guided fuzzing of the wire codecs: the moq-lite and moq-transport control
messages, stream and datagram headers, both varint codecs, and `Path`. These are the
bytes a relay parses from an untrusted peer, so they are the surface worth fuzzing.

```bash
cargo install --locked cargo-fuzz
just rs fuzz lite
```

The targets are `lite`, `ietf`, `varint`, and `path`. Extra arguments pass through to
libFuzzer, so `just rs fuzz lite -- -max_total_time=300` bounds a run.

## How it fits together

The target bodies are not in `fuzz_targets/`; they are in `moq-net`'s hidden `fuzz`
module (`src/fuzz.rs`), and the files here are one-line shims. Two reasons:

- `lite` and `ietf` are private modules, so an outside crate cannot reach a single
  decoder.
- `just test` replays the same bodies on the pinned stable toolchain, so a crash found
  here becomes a regression test that CI runs without anyone installing cargo-fuzz.

`just rs fuzz` regenerates `seeds/` from `moq_net::fuzz::seeds()` before each run, so
the corpus follows the dispatch instead of rotting beside it. libFuzzer writes what it
discovers to `corpus/<target>/`, which is kept across runs and gitignored along with
`seeds/` and `artifacts/`. `cargo +nightly fuzz cmin --fuzz-dir rs/moq-net/fuzz <target>`
shrinks the corpus once it has grown.

## What the targets assert

Beyond "does not panic":

- Whatever we encode, we decode back, consuming every byte. The peer's decoder is this
  same code, so emitting something we would reject is a bug on the wire.
- Encoding is stable: encode, decode, encode again yields identical bytes. Skipped
  where a parameter map reaches the wire, since those encoders walk a `HashMap` and its
  iteration order differs per instance (moq-lite SETUP, and draft-14/15, which unlike
  draft-16+ do not sort by key first).
- `Path::relative` inverts `Path::resolve`, and never produces a reference that walks
  above the root.

A decode that fails is not a finding, and neither is an encode that fails: a value can
decode at a version that cannot express it again.

## When something crashes

libFuzzer writes the input to `artifacts/<target>/crash-<hash>` and prints the command
to reproduce it. Fix the bug, then commit the input:

```bash
cp rs/moq-net/fuzz/artifacts/lite/crash-<hash> rs/moq-net/fuzz/regressions/lite/
```

`fuzz::tests::regressions` replays everything under `regressions/<target>/` as part of
the normal test suite, so the case stays covered.
