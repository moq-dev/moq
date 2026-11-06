# [M] moq-c at parity for the cut

## Goal

The hand-written `moq-c` crate (`rs/moq-c`, `moq.h`) ships at this cut at the
shared 0.22 minor with every binding feature moq-ffi gains in m1 or m2, so a C user
can do whatever a Python, Swift, Kotlin, Go, Dart, or C++ user can. The C
interop client and every `doc/lib/c` sample build and run against the built
release archive before the cut.

## Plan

Decided 2026-10-25 (maintainer): the hand-written crate ships at this cut and
the [generated C line](/quest/m3/c/README.md) moves after it, waiting on the
switch to rust2's UniFFI fork. The generated package (`c/moq`) builds in CI,
but it publishes only after the cut. C users break
twice (to 0.22 at this cut, then to the generated package at a later cut);
that is the accepted cost. moq-c follows the shared minor like every
published package (CONTRIBUTING.md's [Versions](/CONTRIBUTING.md#versions),
[moq-dev/moq#7610](https://github.com/moq-dev/moq/pull/7610)), not a 0.7
then 0.8 line of its own.

- moq-c is not frozen: it takes the same breaks as moq-ffi and the other
  bindings. Every m1 or m2 quest or PR that changes the binding API changes
  `rs/moq-c` in the same PR, with its Impact migration note and `doc/lib/c`
  update; none waits on the generated package.
- Before the cut, diff the moq-ffi exports against `moq.h` and add any m1 or m2
  binding feature still missing, so parity is checked rather than assumed.
- `rs/moq-c` publishes through release-plz like the other crates; confirm the
  release produces the C archive the samples are checked against.

Public API: moq-c gains each m1 or m2 binding feature. Wire: none.
