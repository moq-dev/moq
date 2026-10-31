# [M] The fill path is linear in range count and span

## Goal

`track_range_filling` (`rs/moq-net/benches/track.rs`) holds roughly flat
groups/s as range count and span grow. The model's cost per group no longer
grows with the number of ranges in a subscription, or that number is bounded
by the design (flow control or an owned budget), not by a refusal constant. A
benchmark sweeps range count and subscriber count, so a slope shows up.

## Plan

Measured in [moq-dev/moq#6722](https://github.com/moq-dev/moq/pull/6722): 234K
groups/s at 1 range × 1 span falls to 1.9K at 256 ranges × 64 span. Each poll's
drift edge (`live_edge`) and each candidate's successor search
(`arrival`/`successor`) walk `lookup` across all of a reader's runs, and a
filled group is never marked shown, so every scan passes it again.
`Misses::retain` also rescans every filled group per poll.

The range count is hostile input too
([moq-dev/moq#6715](https://github.com/moq-dev/moq/pull/6715)): lite-07
SUBSCRIBE carries a list of ranges, a peer can send as many as fit in one
message, and the model's per-group cost grows with that count, so a hostile
peer can multiply the CPU a relay spends on every group of a track.

- Measure first: extend `rs/moq-net/benches/track.rs` with ranges ×
  subscribers beside `track_range_filling`'s ranges × span.
- Prefer merging overlapping or adjacent ranges at decode, and a lookup that
  touches only the ranges around a group.
- Include `track::Subscriber::covered`, which the lite-07 publisher runs on
  each idle wake of a bounded subscription and walks every range.
- Include the lite-07 publisher's `Frames::start`/`Frames::end`
  (`rs/moq-net/src/lite/publisher.rs`), which scan every decoded range per
  served group and per datagram; key them by group (the 2026-10-27 security
  audit's NET-1 found about 16k ranges fit one SUBSCRIBE).

Decided 2026-10-27 (maintainer):

- The range count cost quest folds in here, since both fix the same walk.
- Re-baseline `track_range_filling` on current `main` before optimizing:
  [moq-dev/moq#6921](https://github.com/moq-dev/moq/pull/6921) changed how
  sparse misses are judged after the numbers above.

[FETCH as a range](/quest/m1/subscribe-ranges/fetch-range.md) waits on this
(maintainer, 2026-10-25, on #6722), since one-range reads would put every
past read on this path.

Public API: none expected. Wire: none expected.
