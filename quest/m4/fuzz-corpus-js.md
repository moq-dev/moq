# [M] js/net decodes the Rust fuzzers' corpus the same way

## Goal

Every Nightly, `js/net` decodes each input in the Rust wire fuzzers' minimized
corpus and agrees with `moq-net` on whether it is accepted and on the bytes it
re-encodes to. A codec change made in one language and not the other fails
Nightly with the input that tells them apart.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan on what could be proved about
moq-net). Milestone m4.

`AGENTS.md` requires a Rust wire change to update JS in the same PR, and
today only a hand-written corpus (`rs/moq-net/fixtures/lite.json`, replayed
by `js/net/src/lite/corpus.test.ts`) checks that they agree. The fuzzers
already produce thousands of inputs at the edges of the decoders; this reuses
them.

- After a fuzz target minimizes its corpus, a small Rust example (beside
  `rs/moq-net/examples/fuzz-seeds.rs`) writes one record per input: the bytes,
  whether the target's decoder accepted them, and the re-encoding. A Bun test
  reads the records and asserts `js/net` gives the same answer.
- Cover each target whose decoder `js/net` also has: the lite messages,
  announces, both varints, paths, and the moq-transport messages JS speaks.
  List the targets left out and why.
- A disagreement is a bug in one codec or the other. Fix it, and commit the
  input to `fixtures/` so `just check` replays it in both languages from then
  on.
- Where the two differ on purpose (a version only Rust speaks, a map whose
  order is not stable), say so in the record's schema, not with a skip list
  in the test.

Public API: none. Wire: none.

## Related

- [Shared corpora live in each crate's fixtures/](/quest/m5/corpus-fixtures.md) - where the committed inputs live
