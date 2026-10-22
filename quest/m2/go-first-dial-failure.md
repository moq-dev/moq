# [XS] Go reports why a first dial fails

## Goal

The Go wrapper reports why a client's first dial is failing, as Python,
Swift, Kotlin, and Dart do since
[moq-dev/moq#6131](https://github.com/moq-dev/moq/pull/6131), and the Kotlin
and Dart docs describe `client.failure()` on the object that actually has it.

## Plan

Decided 2026-10-21 with the maintainer: m2. #6131 left Go without a `Client`
failure accessor; add it the way the other wrappers expose it.

[moq-dev/moq#6170](https://github.com/moq-dev/moq/pull/6170) found that
`doc/lib/kt/index.md` and `doc/lib/dart/index.md` say `client.failure()`
while their examples only use `Moq.connect`, which never hands back a
`Client` before connecting. Align that wording.

Public API: Go additive. Wire: none.
