# [S] The congestion selector ships BBR, and new algorithms join it additively

## Goal

The cut ships one public congestion-control selector that applications, the
relay's config, the CLI, and every binding use, with BBR (v3) selectable and
the default. Algorithms still under test (GCC, SCReAMv2, and BBR3's delay
bound) are not reachable from any published crate's public API, and each can
join the selector later without a breaking change.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
GCC (`rs/moq-congestion/src/gcc/`, about 16.3k vendored lines), SCReAMv2
(`scream.rs`), and BBR3's delay bound (`bbr3::Config::delay_bound`) from
[moq-dev/moq#7546](https://github.com/moq-dev/moq/pull/7546),
[moq-dev/moq#7547](https://github.com/moq-dev/moq/pull/7547), and
[moq-dev/moq#7544](https://github.com/moq-dev/moq/pull/7544) are public in
moq-congestion 0.1.0, which has never been published, yet no session can
select them, and nothing outside tests turns on the receive timestamps GCC
needs.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)),
in the maintainer's words: "Add a way to select them? We need BBR for sure,
while GCC and Scream can wait for more testing I guess. So long as new
algorithms are additive." ✅ A public selector ships BBR; GCC and SCReAM are
not selectable in the cut and stay out of the published surface until tested
(rejected: delete them now and restore them for the bake-off; ship them
behind an unstable feature; publish them as they are).

Choices made with that pick, for review:

- The selector already exists, so extend it rather than add a second one:
  `moq_tokio::quic::CongestionControl` (`#[non_exhaustive]`, with `Loss`
  for CUBIC, `Delay` for BBR v3 and the default, and `None`), the relay and
  CLI's `congestion_control` key, and moq-ffi's `MoqCongestionControl` in
  every binding. `quic::Controller` stays the escape hatch for an
  application's own controller. Its spellings ship on `release` (`delay` and
  `loss` in relay configs), so keep them rather than break every deployed
  config, and name each new variant after its algorithm (`gcc`, `scream`).
  The docs say plainly that `delay` is BBR v3. A generated enum that a
  language switches over exhaustively (Swift, Kotlin, Dart) makes a new
  variant a break for its users, so give `MoqCongestionControl` a shape each
  binding can grow without one before the cut.
- The delay bound is a BBR option, not a variant of its own: it bounds BBR3's
  inflight from the forward-delay trend inside BBR's own state machine, so a
  separate variant would duplicate BBR's settings under a second name, while a
  field on a non-exhaustive `bbr3::Config` is additive. Until it is tested,
  its setter is not public; it graduates as that option, or as BBR's default
  if the bake-off shows it wins.
- Keep GCC and SCReAM in the tree but off the published API. Recommended:
  move them into a workspace-only crate (`publish = false`) that the
  bake-off's sim and integration tests use as a path dev-dependency, which
  `cargo publish` drops; the vendor-sync quests follow the code.
- JS has nothing to select: browsers choose WebTransport's congestion
  control, and `js/net` already asks for `low-latency`.

[Realtime QUIC](/quest/m4/quic-realtime.md) is the bake-off that tests the
candidates and adds each winner to the selector as a variant (or the delay
bound as a BBR option).

Public API: removes `moq_congestion::gcc`, `moq_congestion::scream`, and
`bbr3::Config::delay_bound` from the public API, none of them yet
published; `CongestionControl` and `MoqCongestionControl` are unchanged.
Wire: none.
