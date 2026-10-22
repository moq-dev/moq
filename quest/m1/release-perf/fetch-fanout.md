# [M] FETCH fan-out

## Goal

When many viewers FETCH the same group through a relay over moq-lite, the
relay fetches it upstream once and serves later viewers from its cache. Cost
per viewer falls as viewers grow, as it does on `release`.

## Plan

Measured 2026-10-21 (`session_delivery_group_cost/.../fetch` on lite-06,
cost per viewer):

| Viewers | release | main nightly |
|---|---|---|
| 1 | 57 us | 67 us |
| 16 | n/a | 61 us |
| 64 | 16.5 us | 59 us |

Main's non-fetch path still amortizes (55 us down to 34 us), so on main every
viewer's FETCH appears to cost as much as the first. The relay seems to go
upstream or rebuild state for each fetch. Lite's `IDLE_LINGER` should keep
the relay's copy and cache, so a simple "the track was dropped" explanation is
not enough.

1. Reproduce first. Count the upstream FETCHes the publisher session sees in
   each round. One per group means the cache works and the cost is elsewhere.
   One per viewer means the cache is bypassed.
2. If it is bypassed, compare callgrind instruction counts at the parents of
   the suspects. The likely fix is for the relay's FETCH serve to resolve its
   track once and reuse the lingering copy. Suspects:
   [#5820](https://github.com/moq-dev/moq/pull/5820) (the relay's FETCH serve
   resolves `broadcast.track(..).fetch_group` per request),
   [#5809](https://github.com/moq-dev/moq/pull/5809) (a fetch keeps its track
   read), [#5680](https://github.com/moq-dev/moq/pull/5680) (fetch age
   budgets; the bench advances the cache clock 1 s per round), and
   [#6020](https://github.com/moq-dev/moq/pull/6020) (one writer per group).
3. Land a regression test that asserts one upstream FETCH per group across
   many viewers. Also check that the `session_delivery_group_cost` lite
   `/fetch` sweep over viewers slopes down again.

A relay that fetches upstream once per viewer turns N viewers into N upstream
requests. If step 1 confirms that, raise the priority with the maintainer,
since m0 bounds what a peer can make the relay do.
