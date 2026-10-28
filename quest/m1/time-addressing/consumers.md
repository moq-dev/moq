# [L] Media consumers address groups by start

## Goal

Every first-party consumer that numbered groups works by start timestamp,
in Rust and JS, and hang stops writing duration and discontinuity markers
that group ends and gaps now carry. The HLS/DASH export lists segments
straight from group starts and ends, so the HLS records go away.

## Plan

Decided 2026-10-25 (maintainer), from the spike:

- GROUP_END replaces hang's empty-frame duration marker
  (`rs/moq-mux/src/container/producer.rs:523`), so moq-mux stops writing
  empty frames.
- Discontinuity marker groups go away (`producer.rs:650`): a gap is a
  previous end below the next start, and a missing group is a
  previous-start link that doesn't match.
- Legacy egress synthesizes neither: old players lose end durations and
  pause markers, which they already tolerate being absent.

Decided 2026-10-27 (maintainer): moq-mux publishes HLS records by default
until this lands. Then the `<track>.hls` record tracks, the catalog `hls`
field (hang draft and both catalog libraries), the `catalog::Config::with_hls`
opt-out, and any A/V segment alignment go away.

Convert each consumer that numbers groups: hang and moq-mux timelines,
HLS/DASH export, `seek` for GOP-aligned failover in moq-mux, HLS import's
discontinuity packing, transcode's 1:1 numbering, archive keys and resume,
relay and CLI `fetch --group`, and player continuity checks in the Rust and
JS container consumers and `js/watch`. Update the hang draft and the docs
each one makes stale; rename CLI flags per the repo's CLI rule.

Added 2026-10-27 (maintainer, from an external release-to-main audit): an
HLS exporter today requests the record tracks, but on-demand capture starts
from media-track demand, so an HLS-only viewer never starts it. Once the
export reads media tracks directly, prove that an HLS-only viewer of an
on-demand publisher starts capture and gets a playlist.

Public API: breaking where a consumer exposes group numbers (CLI
`--group`, archive keys), and `moq_mux::hls`, `hang::hls`, and
`catalog::Config::with_hls` are deleted. Wire: hang drops empty duration
frames, discontinuity marker groups, the `hls` catalog field, and the record
tracks.

## Required

- [Rust groups by start](/quest/m1/time-addressing/model.md) - the Rust API these consume
- [@moq/net by start](/quest/m1/time-addressing/js.md) - the JS API these consume
