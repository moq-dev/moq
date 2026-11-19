# [M] A JS dropped group costs a subscription what it changes

## Goal

In `@moq/net`, a drop that starts one time range of a subscription with many
ranges costs about what it does with one, as it does in Rust since
[moq-dev/moq#9142](https://github.com/moq-dev/moq/pull/9142). A JS bench sweeps
drop cost over ranges and drops, so a regression shows in Nightly's JS
benchmarks.

## Plan

`js/net/src/window.ts` still has the shape Rust's `Window` had before the fix:
each settle tries every waiting time start (`#startWaiting`), and any move
rebuilds and re-sorts every span's runs (`#rebuild`), so each drop that
places one range costs every range. Port the Rust fix:

- Waiting spans keyed by time, tried only outside the runs a new optional
  `Holdings.unplaced` reports, with a mark that changes when a lite-07 copy's
  reported time starts, `atKeys`, or anchoring change. The track's holdings
  in `js/net/src/track.ts` need the dropped prefix in one lookup: `DropSet`
  keeps runs with different errors apart, so add a coalesced key set as
  Rust's `TrackState::dropped`.
- A span that starts or whose lookup answers inserts its runs into the sorted
  runs instead of rebuilding them; only a span that starts looking up its
  start rebuilds.
- `start()` from counted starts rather than a walk of the spans.

Then measure what else in `track.ts` a window move costs per drop. JS has no
`DropCursor`, so check its drop reporting the same way.

Public API: none. Wire: none.
