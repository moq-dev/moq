# [M] A relay holding the track names a live join's start once upstream places it

## Goal

A lite-07 subscription carrying a time range and a live join, such as `1000..=2000` plus
`{ live: 1000 }`, delivers the join's groups to a reader one hop past a relay whose copy
already holds the track, Rust or JavaScript, as it does direct and through a cold relay.
Today the Rust relay's wire
publisher sends SUBSCRIBE_START as soon as its copy hands out the time range's first group,
naming no start for the join (`starts: [Some(1000), None]`) while the join still waits on its
upstream answer, so the reader drops the join and gets only `[1000, 2000]`.

## Plan

- Reproduce: drop the `chain` marker on "a time range and a live join in one subscription"
  in `rs/moq-net/tests/range-table.json` and run `range_table::chain`.
- Start in `lite/publisher.rs` `TrackRun::starts`, whose `(None, _) => return None` arm
  answers a range still waiting on the live feed as empty. A SUBSCRIBE_START can't wait for
  every join without holding back the groups already servable, so the answer likely names
  such a range later, as SUBSCRIBE_STARTED does for a range an update added; that may need
  a lite-07-wip wire change and the matching draft edit.
- The JavaScript relay fails the same case (the `relay-warm` marker, `just test relay-fill`),
  also on `main` before this marker landed; fix both and drop both markers. The bun
  suite's `warm` mode and `relay-cold` both pass it, so the JavaScript divergence lies in how
  the relay serves its warm copy to the next Rust hop (`js/net/src/lite/publisher.ts`), not
  in the copy: it answers the time range from what it holds and must still ask upstream for
  the join.

Public API: none. Wire: possibly, under lite-07-wip.
