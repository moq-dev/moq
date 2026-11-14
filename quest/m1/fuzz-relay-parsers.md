# [M] The relay's WebSocket and token parsers have fuzz targets

## Goal

`qmux`'s framing (over WebSocket and TCP) and `moq-auth`'s token and key
parsing each have a cargo-fuzz target that asserts an invariant, runs in
Nightly's fuzz matrix, and replays its committed regressions in `just check`.
These are the two parsers a relay exposes to any peer that have none today.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan on what testing to add): 24 targets cover moq-quic, moq-net, moq-mux, and moq-rtmp. Of the
parsers without one, `rs/moq-relay/Cargo.toml` links only `qmux` and
`moq-auth`, so they come first and apart from
[the gateway and format parsers](/quest/m4/fuzz-gateway-parsers.md).

- ✅ m1 (rejected: m4; all new testing quests in m1).

Agent's choices, for the implementer to revisit:

- Follow moq-net's shape (`rs/moq-net/fuzz/README.md`): the body in a `fuzz`
  module behind the `fuzz` feature, a one-line shim per target, an
  `--exclude` in `rs/justfile`'s `no_fuzz` list, and a row in `nightly.yml`.
- qmux: a whole-session target as moq-net's `session_lite` is, feeding
  arbitrary frames and asserting that what a peer makes the session hold
  stays within its limits, and that accepted streams round-trip their bytes.
- moq-auth: arbitrary tokens and keys never verify without the signing key,
  and whatever decodes re-encodes to the same claims. Commit any input that
  tells Rust and `js/auth` apart to the shared corpus.
- Report what the first runs found; each finding is fixed in the PR or gets a
  quest.

Public API: none. Wire: none.

## Related

- [OSS-Fuzz](/quest/m4/oss-fuzz.md) - runs these around the clock once it lands
