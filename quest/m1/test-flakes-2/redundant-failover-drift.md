# [S] Redundant TS failover survives a killed importer behind bufferbloat

## Goal

moq-cli's `export::redundant_importers_switch_through_bufferbloat_on_lite_07`
(`rs/moq-cli/tests/integration/export.rs`) passes reliably, fixed at its
cause. On current main it failed 3 of 10 isolated local runs and 2 of 3
loaded `just check` runs with "stitching export exited". It is also the
slowest test in the queue suite (37 s, past nextest's 30 s SLOW mark).

## Plan

Found by the 2026-10-19 test cost audit, with the exporter's stderr captured
on a failing run. After the test kills the newest importer, the exporter
fails instead of switching back to the survivor:

> export failed with the broadcast still up, so not lingering ... the
> source's clock runs -6129221.2 ppm off ours, past the 30 ppm the output's may follow

A killed importer sends no close, so the relay keeps its route until the QUIC
idle timeout (10 s by default). The viewer meanwhile gets no media; behind the
160 kbit/s shaper the backlog adds to that stall. The jitter clock in
`rs/moq-mux/src/jitter.rs` charges the stall to source drift and gives up
once it has cost half the delay. The unshaped sibling shows the same stall as
an 11 s output gap at its second switch, but passes.

Decide which side is wrong before changing the test, recommended first:

1. The jitter clock should not read a stalled source as drift. A stall is
   missing media, which the follower can wait out or switch away from.
2. The follower should switch to the live survivor once the stalled route's
   media is stale, without waiting for the relay to drop the route.
3. Both are right, and the export legitimately fails behind a link below the
   media rate. Then the test's scenario needs another shape.

Giving the importers `--quic-idle-timeout 2s --quic-keep-alive 500ms`
passed 5 of 5 runs and cut the three redundant-importer tests from 15, 29 and
37 s to 4, 16 and 30 s. That only shortens the stall, so land it after the
cause is fixed, not instead of it.

Public API: none. Wire: none.
