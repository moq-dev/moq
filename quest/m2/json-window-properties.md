# [S] Window codec property tests

## Goal

`@moq/json` and `moq-json` each have a property test that applies random
sequences of window pushes and pops (with resets and group rolls) through the
Window `Encoder` and `Decoder`, and asserts that every reader's push, pop, and
skip events match an independent model of the logical window. A reader keeping
up sees the producer's window exactly. A reader joining at a group adopts the
header's `offset` and skips only `offset..start`; one that falls behind skips
only what it never received. It runs on every PR and in the nightly. No change
to the codec itself unless the test finds a bug.

## Plan

Decided 2026-10-14, alongside
[Snapshot codec property tests](/quest/m2/json-snapshot-properties.md),
which carries the reasoning for each choice:

- **Codec sequences** through `Encoder` -> `Decoder`, per implementation, at
  the level a consumer sees: push, pop, and skip events. The oracle is a
  separate model of the logical window, not the encoder's own state, which
  with `checkpointRecords` holds only the checkpoint suffix.
- **Generators** cover record values with `null` at any depth, `opRatio`
  rolls, `checkpointRecords` bounds, and readers that join late or fall a
  group behind.
- **CI:** fixed seed and modest case count on PRs, random seed and more cases
  nightly. A nightly failure's shrunk case lands with its fix as a replayable
  op trace (codec config, ops, resets, reader join and drop points).
- **Shared vectors assert decoding.** The `window-vectors.json` loops in
  both suites check wire bytes and that frames decode, but not the decoded
  events. Add expected events per vector, including the bounded-checkpoint
  `skip`, so a vector can pin a decode bug.
- **Same PR for both languages** so the two generators stay mirrored.
- **Open: library.** Reuse whatever the Snapshot quest picks; fast-check and
  proptest are suggested.

## Related

- [Snapshot codec property tests](/quest/m2/json-snapshot-properties.md) - the same treatment for the Snapshot codec
