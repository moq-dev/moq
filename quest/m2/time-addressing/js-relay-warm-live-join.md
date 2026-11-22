# [M] A JavaScript relay's warm copy answers a live join beside a time range

## Goal

A lite-07 subscription holding both a time range and a live join, such as
`1000..=2000` and `{ live: 1000 }`, delivers every group of both through a
JavaScript relay whose copy an oldest reader already filled, as it does direct, through
a cold JavaScript relay, and through a warm `@moq/net` copy within one language. Today
`relay-warm` in `just test relay-fill` delivers only the time range's groups (`1000`,
`2000`) and never the live join's (`8000`, `9000`).

## Plan

- Reproduce by dropping the `relay-warm` marker on "a time range and a live join in one
  subscription" in `rs/moq-net/tests/range-table.json` and running `just test relay-fill`.
  It fails on main since [moq-dev/moq#9316](https://github.com/moq-dev/moq/pull/9316) added
  the case; the bun suite's `warm` mode passes it, so the divergence lies in how the
  relay serves its copy to the next Rust hop (`js/net/src/lite/publisher.ts`), not in the copy.
- Compare with `relay-cold`, which passes: the warm copy answers the time range from what it
  holds, and the live join, which only upstream answers, must still be asked.

Public API: none. Wire: none expected.

## Related

- [A relay serves a time range inside one group to the next hop](/quest/m2/time-addressing/relayed-range-in-one-group.md) - the Rust relay's own divergence in the same table
