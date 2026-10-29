# [S] Bindings default to zero max delay

## Goal

A subscription built with no arguments skips superseded groups at once in
every language, as Rust, JS, Python, and the released bindings do. `null`
(Rust `None`) keeps meaning "no subscriber limit", distinct from zero, and a
reader asks for it explicitly.

## Plan

Decided 2026-10-27 (maintainer), from an external release-to-main audit
that found the regression in Dart; it reaches every binding that exposes
the generated record directly.

- Release generated `MoqSubscription(maxAgeUs: 0)`. Today
  `MoqSubscription.max_delay` is `Option<Duration>` with no UniFFI default,
  because UniFFI cannot spell a duration literal
  (`rs/moq-ffi/src/consumer.rs`), so a bare constructor sends `None`. moq-net
  reads `None` as a `Duration::MAX` budget
  (`rs/moq-net/src/model/subscription.rs`), while its own
  `Subscription::default()` is `Some(Duration::ZERO)`. An unchanged
  `Subscription()` therefore compiles and silently trades latency for
  delivery of every retained superseded group.
- Keep both meanings: zero is the default, `None` is no subscriber limit.
  Rejected: making `None` the default everywhere, since it changes Rust and
  JS live readers too and adds latency after congestion.
- Python already wraps the record with `max_delay = timedelta(0)`
  (`py/moq-rs/moq/_records.py`); follow it. Dart
  (`dart/moq/lib/src/aliases.dart`), Swift (`swift/Sources/Moq/Aliases.swift`),
  and Kotlin (`kt/moq/src/jvmAndAndroidMain/kotlin/dev/moq/Aliases.kt`)
  alias the generated record, so each gets a hand-written `Subscription`
  whose default is zero. Go's `Subscription` (`go/wrapper/records.go`) is a
  struct whose zero value leaves `MaxDelay` nil; pick a Go shape whose zero
  value means zero delay and name the no-limit spelling, and raise that
  shape in the PR as the API decision. Check C++ aggregate initialization
  in `cpp/moq` and the `moq-c` subscription the same way.
- Audit the other binding records whose `max_delay` mirrors a moq-net
  default (fetch options, decoder outputs) against release and fix any that
  drifted the same way.
- Test that each wrapper's no-argument subscription reaches moq-net as
  `Some(0)`.

Public API: breaking for Dart, Swift, Kotlin, and Go, whose `Subscription`
changes type or default; the Impact bullet says a reader that wants no
subscriber limit now passes it explicitly. Wire: none.
