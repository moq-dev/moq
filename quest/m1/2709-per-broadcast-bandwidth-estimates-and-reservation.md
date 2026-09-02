# [L] js/net: mirror the send-side bandwidth allocator

## Goal

On a connection shared by several components (#2705), each JS publisher's
encoder targets its own share of the send-rate estimate instead of the
whole-session number, and audio reserves its bitrate so video's share is
honest. The split follows the Rust allocator exactly: strict priority tiers by
track priority, max-min fair within a tier, surplus left unclaimed.

Boundaries: the receive side is untouched. Watch ABR keeps reading the
whole-connection PROBE estimate until Rust has a receive-side split to mirror.

## Plan

The Rust side landed on `dev` in `rs/moq-net/src/model/bandwidth.rs`
(#2854): `Allocator::new(estimate)` / `unlimited()`, `reserve(&track::Demand,
max) -> Reservation`, `Reservation::{peek, consumer, update}` with `Drop`
removing the entry, and a pure `allocate(estimate, wants, id) -> Option<Rate>`
where `None` means "not registered, hold your rate" and is distinct from a
zero grant. Only tracks whose demand is active claim anything. That module is
the spec; JS has no equivalent today.

What JS does today: `js/publish/src/element.ts` polls `connection.stats()`
every 100 ms into one signal and hands it to the video encoder only, which
caps at `estimate * 0.9`; audio is invisible to it. `js/net` exposes
`Stats.estimatedSendRate` and the PROBE `estimatedRecvRate` but nothing
divides either.

Steps:

- Add a `bandwidth` module to `js/net` porting `Allocator`, `Reservation`,
  and `allocate()` one to one, including the Rust unit tests. Rates are bits
  per second as plain numbers. A reservation exposes its grant as a
  `Getter<number | undefined>` so encoders react through the signal model.
- Move the send-estimate sampler from `js/publish` into `js/net`: the
  established connection owns one 100 ms `getStats()` loop and one
  `Allocator`, shared across reloads, so every component on the connection
  reserves against the same registry.
- Demand comes from the `js/net` track producer (active vs idle) and priority
  from the track info the publisher set (`js/hang` `PRIORITY`: catalog 100,
  audio 80, video 60), the same tiers Rust allocates on.
- `js/publish` video takes its reservation's grant instead of `estimate *
  0.9`; audio reserves at its configured bitrate and ignores the grant, which
  matches Rust's audio today. Following the grant for Opus is the JS twin of
  [#2848](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md)
  and stays out of scope.

Tests: the ported `allocate()` cases verbatim; an integration test with two
publish components on one connection whose grants sum to at most the estimate
and rank by priority; a test that an idle track claims nothing.

Branch from `dev`, where the shared connection and `forward.ts` live.

## Closes

- [#2709](https://github.com/moq-dev/moq/issues/2709) - close this issue when the quest finishes

## Related

- [#2848](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - audio following its grant, the Rust half
- [#2859](/quest/m1/2859-passthrough-imports-reserve-no-bandwidth-so-a-co-resident.md) - passthrough imports reserving nothing
- [#2857](/quest/m1/2857-bindings-cant-reach-encoder-rate-control-so-every-non.md) - the same gap for the native bindings
