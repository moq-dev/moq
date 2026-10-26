# [S] The JS gap ledger costs O(log n) per sparse arrival

## Goal

`@moq/net`'s gap `Ledger` handles sparse arrivals in O(log gaps) and expires
in O(1) when nothing is due, like Rust's since
[moq-dev/moq#6287](https://github.com/moq-dev/moq/pull/6287). A sparse or
hostile publisher can no longer drive quadratic main-thread CPU, and a sweep
benchmark shows the flat slope.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Reproduced, inherited
(main widened it to every track).

`js/net/src/ledger.ts:38-85` rebuilds its array in `account` off the in-order
fast path, and `expire` rebuilds it on every arrival because `tail.ts:109`
calls it unconditionally. It runs per datagram (`lite/subscriber.ts:1714`),
per group stream (`:1592`), and from `track.ts:1637`. Sequences 0, 2, 4, ...
within `maxDelay` measured 81 ms for 2k arrivals, 460 ms for 8k, and 12.1 s
for 32k.

Mirror the Rust ledger: sorted runs with binary search, and a cached
earliest-fold instant so `expire` returns at once when nothing is due. Add a
sweep to `js/net/bench/` over arrival count. Verify in Chromium per the
repository's browser benchmark path.

Public API: none. Wire: none.
