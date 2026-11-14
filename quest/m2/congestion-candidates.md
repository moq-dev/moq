# [S] Decide whether moq-congestion publishes its bake-off candidates

## Goal

moq-congestion's first publication carries only controllers a session can
select, unless the maintainer decides to publish the candidates, and the
vendor-sync quests stop maintaining code nothing runs.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
GCC (`gcc/`, about 16.3k vendored lines), SCReAMv2 (`scream.rs`), and BBR3's
delay bound (`bbr3/delay.rs`) from
[moq-dev/moq#7546](https://github.com/moq-dev/moq/pull/7546),
[moq-dev/moq#7547](https://github.com/moq-dev/moq/pull/7547), and
[moq-dev/moq#7544](https://github.com/moq-dev/moq/pull/7544) are public in
moq-congestion 0.1.0, which has never been published. Production selects only
Loss, Delay, and None (`rs/moq-tokio/src/quic/backend.rs`,
`rs/moq-uring/src/quic/mod.rs`); nothing outside tests sets `delay_bound` or
receive timestamps, so an embedder installing GCC gets its round-trip
fallback, the case the receive-timestamp spike measured as worse. They cost
two recurring vendor-sync quests in m3 and about 28 ignored tests. The
bake-off's consumer, [realtime QUIC](/quest/m4/quic-realtime.md), moved to m4
on 2026-11-12, which the 2026-10-17 bake-off decision did not know.

Recommended: (a). Delete the three candidates and their sim and integration
tests; move [webrtc vendor sync](/quest/m3/vendor-sync/webrtc.md) and
[scream vendor sync](/quest/m3/vendor-sync/scream.md) under the m4 bake-off,
which restores the candidates from git when it runs.

Public API: (a) removes `moq_congestion::gcc`, `scream`, and BBR3's
`delay_bound`, unpublished. Wire: none.

## Waiting

- Maintainer: (a) delete them now and restore them for the bake-off (recommended); (b) keep them behind an unstable feature; (c) publish them as they are
