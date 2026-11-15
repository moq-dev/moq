# [XS] Binding playout-delay tests stop racing the native wall clock

## Goal

The Go, Swift, Kotlin, and Dart "audio consumer reports its playout delay"
tests no longer hang when the host stalls between the writes and the first
read, matching the Python and Rust fixes
([moq-dev/moq#8050](https://github.com/moq-dev/moq/pull/8050),
[moq-dev/moq#8566](https://github.com/moq-dev/moq/pull/8566)).

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
A fixed consumer with `max` zero skips a group by the native wall clock, so a
read that starts more than the 80 ms warmup after group 1 arrives waits out its
timeout (#8566 reproduced it with a 200 ms stall). Still latent in
`go/wrapper/moq_test.go` (`Delay{Min: 80ms}`, zero Max), Swift's
`SmokeTests.swift` (`max: 0`), and Dart's `moq_test.dart` (`max` defaults to
zero); Kotlin's
[moq-dev/moq#8475](https://github.com/moq-dev/moq/pull/8475) kept `max = 0`
and added a writer that loops until a read lands, which is a retry around the
cause. Pass `max: None` (or the language's unset value) to both consumers, and
delete Kotlin's writer loop.

As a rule for new tests, assert native timing and decoding once,
in moq-ffi's Rust tests, and limit wrapper tests to the code each wrapper
owns (record mapping, cancellation, async adapters, lifetimes). Six copies of
this test took three separate fixes over a week, and three copies were still
unfixed.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) fix all four copies, and from now on native timing and decoding are asserted once, in moq-ffi's Rust tests, while wrapper tests cover only the code each wrapper owns; this quest writes that rule into `rs/moq-ffi/AGENTS.md` (rejected: (b) delete the four copies now).

Public API: none. Wire: none.

