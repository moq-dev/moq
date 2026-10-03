# [L] Two TS export legs emit identical packets for ST 2022-7

## Goal

Two `moq export ts --sync` legs of one broadcast, joined at different
moments, emit packet-identical TS from their first common packet, across
skips and source drift, continuity counters included. A ST 2022-7 selector
can then switch between them hitlessly.

## Plan

Decided (2026-10-01), from t0ms's review of the fixed-delay export (#4645).
They call 2022-7 a differentiator for primary distribution, not a
requirement, so this waits until the T-STD line settles.

- Measured acquisition stays the default. `--sync` opts in to anchoring on
  the catalog clock: release at `clock.wall + pts + delay`, so every leg
  anchors the same way however it joined. This needs NTP-synced hosts. If the
  measured age is implausible (negative, or beyond the delay), the export
  fails loud.
- Continuity counters restart at each group. Each media PID's first packet in
  a group sets `discontinuity_indicator`, and its counter is numbered from
  there, so it comes from the media and not from what a leg has sent. The PCR
  moves to its own PID, because a discontinuity flag on the PCR PID would
  declare a time-base break. Loss is still detected within a group.
- Once anchored identically, both legs must make the same skip decision and
  lay out the same slots. t0ms's `two_legs_render_a_skip_the_same_way` stays
  ignored in #4645 until this lands. With the audio late, it also showed a
  PES header byte that differs at the skip; find and fix that.
- Test with mocked time:
  - two legs, joined a second apart, give byte-identical output including
    the counters;
  - the same across a mid-run skip, and under ±30 ppm drift;
  - `--sync` with an implausible age fails loud.

## Required

- [T-STD TS export](/quest/m1/tstd/README.md) - the export, clock recovery, and schedule these legs share

## Related

- [TS passthrough](/quest/m1/ts-passthrough.md) - byte-identical legs for free in its own lane; adopts `--sync` for alignment
