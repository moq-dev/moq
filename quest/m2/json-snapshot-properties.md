# [S] Snapshot codec property tests

## Goal

`@moq/json` and `moq-json` each have a property test that pushes random
sequences of JSON values through the Snapshot `Encoder` and `Decoder` and
asserts the decoder reconstructs every value exactly. It runs on every PR and
in the nightly. No change to the codec itself unless the test finds a bug.

## Plan

Why: #5513 fixes a JS encoder that dropped a `null` nested in an object it
wrote whole, because merge patch reads `null` as a deletion. The decoder
silently disagreed with the producer until the next snapshot group. The
shared `rs/moq-json/tests/vectors.json` pins the cases found so far, but only
the ones someone thought of.

Decided 2026-10-14:

- **Codec sequences, not diff pairs.** The property runs whole value
  sequences through `Encoder` -> `Decoder`, so it covers `diff`/`merge`,
  forced snapshots, `deltaRatio` rolls, and compression together, at the
  level a consumer sees. A separate pure `diff`/`merge` pair property was
  rejected: the codec property already catches what it would, and shrinking
  keeps the failing case small.
- **Per implementation.** Each language checks its own round trip. Asserting
  that JS and Rust reach the same snapshot/delta decision on random inputs
  was rejected for needing a cross-process harness; the shared vectors keep
  covering agreement on known cases.
- **Generators** must reach the edge cases of merge patch: `null` at any
  depth inside objects and arrays, empty objects, a key changing type
  (scalar, array, object), keys added and removed, and non-object roots.
- **CI:** PR runs use a fixed seed and a modest case count, so the inputs
  only change when the generator does, and a PR goes red only when it changes
  the outcome of one of those cases. The nightly runs a random seed and more
  cases. A nightly failure's shrunk case lands with its fix: as a vector in
  the shared fixture when it reduces to one value pair, else as a replayable
  codec sequence with its config.
- **Equality** is over the JSON value domain (what serializes), not arbitrary
  host-language values.
- **Same PR for both languages** so the two generators stay mirrored.
- **Open: library.** Suggested: fast-check (JS) and proptest (Rust), which
  shrink failures and print a replay seed. Neither is in the repo yet;
  settle it when implementing.

## Related

- [Window codec property tests](/quest/m2/json-window-properties.md) - the same treatment for the sliding-window codec
