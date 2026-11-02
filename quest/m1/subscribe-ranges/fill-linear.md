# [M] A subscription's range count cannot multiply per-poll cost

## Goal

The model's cost per poll no longer grows with the number of ranges in a
subscription, or that number is bounded by the design (flow control or an
owned budget), not by a refusal constant. A benchmark sweeps range count for
each path below, so a slope shows up.

## Plan

The fill path itself went linear in
[moq-dev/moq#6995](https://github.com/moq-dev/moq/pull/6995):
`track_range_filling` holds flat over 1 to 256 ranges, any span, and up to 64
readers sharing the fills, and the serving path's window end and coverage
checks stopped walking every range. Four walks still grow with range count.
A lite-07 SUBSCRIBE fits about 32K ranges in its 64 KiB message, so a hostile
peer can multiply each by that.

- **Time anchors.** While any time end is unsettled, `Window::settle` advances
  every span on each cursor sync, and `Window::watch_starts` and
  `Window::speculative` walk every span on each park. A peer naming many time
  ends past the live edge keeps them unsettled. Timed datagram admission
  (`Window::admits_datagram`) walks every range per datagram. Index the
  unsettled ends by the sequence each waits on, so a settle advances only the
  ends new groups reach, and split datagram admission by anchor kind into
  sorted lookups.
- **Outstanding fills.** While fills are out, each probe runs
  `Misses::expire` over every wanted run and `Misses::resolve` collects the
  joins over every wanted run, so a reader whose fills never answer pays
  O(wanted runs) per live group. Expire by deadline as `Deferred` does, and
  resolve only the joins whose outcome changed.
- **Sparse live numbering.** `TrackState::find_servable` leapfrogs runs and
  live groups, so on a track with sparse live sequences (moq-transport), a
  reader with ranges in the gaps costs O(min(ranges, gaps)) per search.
- **Lite-07 frame bounds.** The lite-07 publisher's `Frames::start` and
  `Frames::end` (`rs/moq-net/src/lite/publisher.rs`) scan every decoded
  range per served group and per datagram. Key them by group. This is the
  2026-10-27 security audit's NET-1, folded in here (maintainer,
  2026-10-27).

Public API: none expected. Wire: none expected.
