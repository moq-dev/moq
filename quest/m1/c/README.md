# [M] moq-c at parity for the cut

## Goal

The hand-written `moq-c` crate (`rs/moq-c`, `moq.h`) ships at this cut as
`moq-c` 0.7 with every binding feature moq-ffi gains in m1, so a C user can do
whatever a Python, Swift, Kotlin, Go, Dart, or C++ user can. The C interop
client and every `doc/lib/c` sample build and run against the built release
archive before the cut.

## Plan

Decided 2026-10-25 (maintainer): the hand-written crate ships at this cut and
the [generated C line](/quest/m2/c/README.md) moves after it, waiting on the
switch to rust2's UniFFI fork. The generated package (`c/moq`) builds in CI,
but it publishes only after the cut. C users break
twice (0.6 to 0.7, then to the generated 0.8); that is the accepted cost.

- moq-c is not frozen: it takes the same breaks as moq-ffi and the other
  bindings. Every m1 quest or PR that changes the binding API changes
  `rs/moq-c` in the same PR, with its Impact migration note and `doc/lib/c`
  update; none waits on the generated package.
- Before the cut, diff the moq-ffi exports against `moq.h` and add any m1
  binding feature still missing, so parity is checked rather than assumed.
- `rs/moq-c` publishes through release-plz like the other crates; confirm the
  release produces the C archive the samples are checked against.

Public API: moq-c gains each m1 binding feature. Wire: none.

## Required

- [API gaps](/quest/m1/c/api-gaps.md) - one malformed-epoch error code, an audio enabled query, and a relay hops test
- [Every consumer cancels while pending](/quest/m1/c/consumer-cancel.md) - catalog, raw, datagram, and media consumers end cleanly on cancel before their subscription is accepted
