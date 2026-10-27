# [L] Media consumers address groups by start

## Goal

Every first-party consumer that numbered groups works by start timestamp,
in Rust and JS, and hang stops writing duration and discontinuity markers
that group ends and gaps now carry.

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

Convert each consumer that numbers groups: hang and moq-mux timelines,
HLS/DASH export, `seek` for GOP-aligned failover in moq-mux, HLS import's
discontinuity packing, transcode's 1:1 numbering, archive keys and resume,
relay and CLI `fetch --group`, and player continuity checks in the Rust and
JS container consumers and `js/watch`. Update the hang draft and the docs
each one makes stale; rename CLI flags per the repo's CLI rule.

Public API: breaking where a consumer exposes group numbers (CLI
`--group`, archive keys). Wire: hang drops empty duration frames and
discontinuity marker groups.

## Required

- [Rust groups by start](/quest/m1/time-addressing/model.md) - the Rust API these consume
- [@moq/net by start](/quest/m1/time-addressing/js.md) - the JS API these consume
