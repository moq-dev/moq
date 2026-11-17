# [S] A fixed-delay export states its whole delay to the relay

## Goal

A subscriber that plays a fixed delay D, such as `moq export ts --delay D`,
subscribes with `Delay { min: D, max: D }`, so the relay resets a group with
`Old` only once it is D old, not D/2. A downstream stall the transport
recovers from well within D costs the export no content to relay resets.

## Plan

Decided 2026-11-17 (maintainer, `/quest-import` of
[moq-dev/moq#9018](https://github.com/moq-dev/moq/issues/9018)):

- ✅ A client whose jitter buffer holds at most D sends `max = D`, and a
  fixed delay D also sends `min = D` (rejected: keep sending the export's
  local skip budget, D/2, as the wire max). The point of the wire max is
  that the relay resets with `Old` instead of sending a group once it is D
  old; sending D/2 makes it give up at half the budget.
- ✅ The relay is unchanged (rejected: also stop resetting lite-06 streams
  that are fully written and only awaiting ACKs,
  [moq-dev/moq#4534](https://github.com/moq-dev/moq/pull/4534)). With D on
  the wire, the relay's rule (expire once D has passed since the next group
  reached it) fires about when the group's last frame misses the
  subscriber's own deadline anyway.
- Out of scope: how fast QUIC recovers and the order it drains a backlog in,
  owned by [export-order](/quest/m3/export-order.md),
  [quic-path-return](/quest/m3/quic-path-return.md), and
  [quic-deadline](/quest/m4/quic-deadline.md).

Facts:

- `ts::Export::budget()` is `program.delay / 2`
  (`rs/moq-mux/src/container/ts/export.rs`), and every track subscription
  the export makes uses it as `Delay::max`, with `min` left at 0.
  `ts::Release` (the `--passthrough` export) does the same at
  `rs/moq-mux/src/container/ts/release.rs:204`. The halving came from
  [moq-dev/moq#4645](https://github.com/moq-dev/moq/pull/4645), where it
  was a local rule: skip a stalled group once it falls D/2 behind the newest
  content, so the group after it has the other half to arrive in.
- In `rs/moq-cli/src/subscribe.rs`, some exports already send
  `min = max = D` (`:449`, `:612`) and others send only `max` (`:366`,
  `:387`, `:467`, `:482`). Audit every fixed-delay subscriber in moq-cli and
  moq-mux the same way, `moq play` included.
- The reporter measured with lite-06, which carries no min on the wire;
  lite-07 carries it as Subscriber Min Age.

Recommendation: the export's local stalled-group skip is the export's own
rule. Keep it only if it still loses no content within D once the wire says
D, judged by mocked-time tests of a stall at a fraction of D. The CLI help
for `--delay` ("skipped once it falls half this behind") changes with it.
Ask the reporter to re-run the blackout sweep from #9018 on the branch.

Public API: none. Wire: none (different values in existing fields).

Caused by, for the fix PR's `## Caused by`:

- [moq-dev/moq#4645](https://github.com/moq-dev/moq/pull/4645) - sent the TS export's local skip budget, D/2, as the subscription's max delay

## Closes

- [#9018](https://github.com/moq-dev/moq/issues/9018) - the relay resets a `--delay 2s` export's groups after a 0.6 s stall

## Related

- [Export order](/quest/m3/export-order.md) - the drain after an outage, the rest of the loss in #8559
- [A hole is waited for by the subscription's delay](/quest/m2/delay-hole-wait.md) - makes the consumer's hole wait follow `Delay::min`, which this sets
