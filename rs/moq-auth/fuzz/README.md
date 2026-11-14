# Fuzzing moq-auth

Coverage-guided fuzzing of what `moq auth serve` parses from any client: the JWT in its
query, the claims inside it, and the JWK or JWK Set its `kid` selects.

```bash
cargo install --locked cargo-fuzz
just rs fuzz token
```

The targets are `token` and `jwk`. Extra arguments pass through to libFuzzer, so
`just rs fuzz token -- -max_total_time=300` bounds a run. The Nightly workflow runs each
for five minutes, resumes from the previous run's minimized corpus, and uploads failure
inputs as workflow artifacts.

## How it fits together

The target bodies are not in `fuzz_targets/`; they are in `moq-auth`'s `fuzz` module
(`src/fuzz.rs`), because they verify without the clock through a private method, and so
`just check` replays them on the pinned stable toolchain.

`just rs fuzz` regenerates `seeds/` from `moq_auth::fuzz::seeds()` before each run: the
tokens the fixed keys sign, every token, payload, key, and set in `tests/corpus.json`, and
the fixed keys alone and as a set. See [moq-mux's README](../../moq-mux/fuzz/README.md)
for the corpus layout and calling `cargo fuzz` directly.

## What the targets assert

The first input byte picks the form: for `token`, a JWT or its JSON payload; for `jwk`, a
key or a key set. Beyond "does not panic":

- A token verifies only under a fixed key that signed its header and payload, and to the
  claims it signed, whether checked against each key or picked from a set by `kid`.
- Claims that decode encode to claims that decode back the same, and a token signed over
  them verifies to them again.
- A key that parses encodes to a key that parses back the same, and verifies a token the
  fixed keys signed only when it holds that signer's key material.
- What a parsed key signs, its public half verifies, to the same claims.

## When something fails

Fix the bug, then commit the input under `regressions/<target>/`, which
`fuzz::tests::regressions` replays as part of the normal test suite. An input that
`js/auth` decides differently from this crate also goes into `../tests/corpus.json`, which
both replay.
