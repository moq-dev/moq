# [M] A datagram sent right behind its answer reaches the subscriber

## Goal

On lite-05, 06, and 07, in Rust and JS, a datagram that arrives in the same
receive pass as its subscription's answer (TRACK_INFO on lite-05/06,
DATAGRAMS_INFO on lite-07) is delivered, not dropped. A datagram the
receiver still cannot route is logged at debug and counted, never dropped
silently.

Not in scope: a datagram whose answer is lost and retransmitted. That is
ordinary datagram loss, and the receiver keeps no buffer for it.

## Plan

Decided 2026-11-05 (maintainer, /quest-import of
[#7882](https://github.com/moq-dev/moq/issues/7882)):

- ✅ **Decode answers before routing datagrams.** The receiver handles its
  pending stream answers before it routes datagrams, so a datagram that
  shares a packet with its answer finds its route. Not chosen: buffering raw
  datagrams per unanswered subscription and replaying them (state a peer
  influences, needing a bound), the publisher holding datagrams until its
  answer is acknowledged (about one RTT of datagrams lost per subscription
  instead), or accepting the loss with only a log.
- ✅ **One quest for Rust and JS.** The JS part builds on the paths
  [JS datagram tracks](https://github.com/moq-dev/moq/pull/7846) introduces.
- ✅ m1, in the Datagram tracks epic.

Verified on `main` at 246916845a, from the issue's report:

- The issue's second question, the `newest - budget` start rule, no longer
  applies. Every version and both languages start datagrams live from the
  request (`rs/moq-net/src/model/datagram.rs`, `js/net/src/track.ts`), so
  this quest leaves the start rule alone.
- Rust: `route_datagram` returns `Ok(())` when `datagram_target` finds no
  route (`rs/moq-net/src/lite/subscriber.rs`). Routes register only once the
  answer decodes: `register` after DATAGRAMS_INFO on lite-07, and
  `legacy_datagrams` after TRACK_INFO on lite-05/06. The driver drains up to
  32 datagrams before polling the source tasks that decode answers, so even a
  datagram sharing its answer's packet is dropped.
- The publisher sends datagrams as soon as the answer is written
  (`rs/moq-net/src/lite/publisher.rs`), so the first live datagram is the one
  at risk.
- JS: `#routeDatagram` returns silently on a miss
  (`js/net/src/lite/subscriber.ts`), and #7846 keeps that.
- No stats count the drop; `datagrams` in `rs/moq-net/src/stats.rs` counts
  deliveries only.

Tests: per version, a datagram written in the same packet as its answer
arrives, and an unroutable one bumps the counter. The integration tests in
`rs/moq-net/tests/integration/datagram.rs` all wait for `consumer.info()`
before appending, so add a case that doesn't.

## Closes

- [#7882](https://github.com/moq-dev/moq/issues/7882) - the datagram served at subscribe time is dropped by the receiver when TRACK and SUBSCRIBE are pipelined
