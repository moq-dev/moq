# [L] JS time values are types, not numbers

## Goal

Every public duration and instant in the published JS packages is a
`Clock.Duration` or `Clock.Instant` object, so a unit mistake fails to
compile and cannot be stripped by `as number` or arithmetic. Today
`@moq/clock` and `@moq/signals` take `Nano`, `@moq/net` takes `Milli`
(`ConnectProps.timeout`, `Backoff`, `Delay`, `Info.maxAge`, fetch `maxDelay`,
`Goaway.handover`, `Drain.timeout`, `Probe.rtt`), and `@moq/qmux` takes raw
unbranded milliseconds (`Config.idleTimeout`, `handshakeTimeout`). That
costs 54 non-test unit conversions in `js/` alone. Covers
`@moq/clock`, `@moq/signals`, `@moq/net`, `@moq/qmux`, `@moq/hang`,
`@moq/json`, `@moq/room`, `@moq/watch`, and `@moq/publish`. `@moq/net`
re-exports `Clock` as it re-exports `Signals`.

## Plan

Decided 2026-11-07 (maintainer, /quest-plan from the API shape audit):
"Can we make a proper new type instead of an alias? Converting to number is
easy but dangerous." ✅ `Duration` and `Instant` classes, mirroring
`kio::clock` (rejected: a `Duration` class with `Instant` kept a branded
number; branded `Nano` everywhere, which keeps the `as number` escape;
moving everything to `Milli`, which reverses #5706 and loses timer
precision).

- `Clock.Duration` is immutable, with unit constructors (`Duration.millis`,
  `secs`, `micros`, `nanos`), unit accessors, `add`/`sub`/`mul`, comparison,
  and no implicit number conversion (`valueOf` refuses). `Clock.Instant`
  supports `add(Duration)`, `since(Instant) -> Duration`, and comparison;
  `Wall` follows the same rule.
- `Clock.Instant`'s arithmetic mirrors `kio::clock::Instant`'s. If
  [moq-dev/moq#8151](https://github.com/moq-dev/moq/pull/8151)'s
  peer-arithmetic drops kio's `Duration` operators for checked and saturating
  forms, JS gets `checkedAdd` and `saturatingAdd` too (added 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests)).
- The wire still carries whole milliseconds; rounding stays inside the
  encoders, as it does today.
- `Manual`, `sleep`, `timeout`, `interval`, and the signals timers take the
  new types.
- Per-frame paths now allocate (watch `Sync`, the audio jitter ring, hang's
  container reader, publish encoders). Run their benches (`js/*/bench`)
  against main and report the deltas in the PR; avoid allocations where a
  hot loop can keep an internal number without exposing it.
- Decide here whether `Time.Timestamp.value` becomes a `bigint` or keeps a
  `number`, together with [time addressing's JS half](/quest/m2/time-addressing/js.md),
  which makes group starts `bigint` timestamps.

Breaking in every listed package; the Impact section gives the migration
(`x as Milli` becomes `Clock.Duration.millis(x)`).

## Related

- [Time addressing in JS](/quest/m2/time-addressing/js.md) - rekeys groups by timestamp in the same packages
