# [M] Congestion control is chosen by algorithm name, from one enum

## Goal

A QUIC endpoint picks its congestion controller by the algorithm's name:
`bbr` (the default), `cubic`, or `unlimited`, in `quic.congestion_control`,
its flag and environment variable, the Rust API, and every binding. One enum
in moq-tokio says so, both runtimes build their controller from it, and no
application can install a controller of its own.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ One enum of algorithm names, no families (rejected: the family names
  `loss`, `delay`, `none` kept; algorithm names beside goal names such as
  `real-time`, which the maintainer raised and then dropped: "just do
  algorithms no families"). A family name stops meaning one thing once two
  delay-based algorithms exist, and the enum's own doc already promises "a
  new algorithm joins as a variant named after it".
- ✅ Delete the moq-uring duplicate and `quic::Config::controller` (rejected:
  keeping the application hook). The hook publishes
  `moq_congestion::Controller` as a plugin interface while that trait is
  still moving.

Facts found by the audit:

- `moq_tokio::quic::CongestionControl { Loss, Delay, None }`
  (`rs/moq-tokio/src/quic.rs:97`) and `moq_uring::quic::Congestion { Loss,
  Delay, None }` (`rs/moq-uring/src/quic/mod.rs:50`) are the same enum, each
  with its own mapping to a factory (`rs/moq-tokio/src/quic/backend.rs:29`,
  `rs/moq-uring/src/quic/mod.rs:62`). moq-relay maps one onto the other
  variant by variant (`rs/moq-relay/src/uring.rs:507-514`), so a new
  algorithm is three edits.
- `quic::Controller` and `quic::Config::controller`
  (`rs/moq-tokio/src/quic.rs:80`, `:248`, `:668`) are set only by moq-tokio's
  own unit tests (`rs/moq-tokio/src/quic/backend.rs:1377`, `:1411`):
  `grep -rn 'controller:' rs` outside moq-quic and moq-congestion finds
  nothing else, and no binding, doc, or downstream quest names it.
  `Controller` gained `on_loss` before `on_ack` on 2026-11-15
  ([moq-dev/moq#8807](https://github.com/moq-dev/moq/pull/8807)).
- The field is `Option<CongestionControl>`, so `None` is "the default, BBR"
  and `Some(None)` is "no congestion control". moq-congestion calls the
  second `Unlimited`.
- GCC and SCReAM sit behind moq-congestion's `lab` feature for the
  [realtime bake-off](/quest/m4/quic-realtime.md). Both are delay-based, so
  the family names cannot name the bake-off's winner.
- moq-ffi already changed the selector's shape in every binding in
  [moq-dev/moq#8850](https://github.com/moq-dev/moq/pull/8850)
  (`MoqCongestionControl`, `rs/moq-ffi/src/session.rs:148`), so the bindings
  take new names at no extra break.
- `loss`, `delay`, and `none` are published: `release` has both enums.

For the PR:

- Name the variants `Bbr`, `Cubic`, `Unlimited`, spelled `bbr`, `cubic`,
  `unlimited`. Keep the enum `#[non_exhaustive]`. The default stays BBR v3,
  and "which BBR" lives in the variant's doc, not its name.
- moq-uring takes the moq-tokio enum or, since its `transport::Config`
  already takes a factory, no enum at all; the relay's variant-by-variant
  match and its `bail!` for an unknown family go.
- Recommended, the maintainer did not pick: `loss`, `delay`, and `none` are
  refused at startup with a message naming `cubic`, `bbr`, and `unlimited`,
  as other removed settings are (`moq_tokio::cli::Deprecated`), rather than
  accepted as aliases. A silent alias keeps the family names alive in every
  deployed config.
- Rename the moq-ffi constructors and every wrapper to match (Swift, Kotlin,
  Dart, Python, Go, C, C++), and their `MIGRATE.md` notes. `@moq/net` has no
  QUIC stack to configure: it passes the browser's own `congestionControl`
  hint (`js/net/src/connection/connect.ts:836`), which is unchanged.
- Update `doc/bin/relay/config.md` (`congestion_control = "delay"` at line
  119, and the `none` paragraph), the CLI docs, and `rs/moq-tokio/MIGRATE.md`.
- Tests: each spelling parses from TOML, flag, and environment; each old
  spelling refuses naming its replacement; an io_uring relay and a tokio relay
  given `cubic` both run CUBIC.

Public API: breaking for `moq_tokio::quic` (`CongestionControl` variants,
`Config::controller` and `Controller` removed), `moq_uring::quic`
(`Congestion` removed), and the moq-ffi selector in every binding. Config
and CLI: `quic.congestion_control` takes `bbr`, `cubic`, `unlimited`. Wire:
none.

## Related

- [Realtime uplink congestion control](/quest/m4/quic-realtime.md) - its winner joins this enum under its own name
