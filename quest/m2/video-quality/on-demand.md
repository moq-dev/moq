# [L] On-demand encoding does not pop between GoPs

## Goal

A viewer of an on-demand encode sees no visible quality jump at a GoP
boundary. That covers a cold start, a resubscribe, and fetched groups spliced
with live ones. The harness grades each of these as a row under the pop
ceiling, and whatever pops it finds are fixed in this quest.

## Plan

Decided in planning (2026-10-28): measure and fix in one quest. If a fix
needs a public API change, such as a new `encode::Config` field, stop and
bring its shape to the maintainer.

Where rate control starts cold today:

- Each live demand session opens a fresh encoder
  (`rs/moq-transcode/src/rung.rs`, `live()`), so the first GoP after any
  subscribe runs without rate-control history.
- Every fetched group opens a fresh pipeline (`rung.rs`, `fetch`), so every
  fetched GoP starts cold. A viewer seeking back sees one cold start per
  group, then a jump into the warm live encoder.
- A color-space change reopens the encoder.
- moq-video's own capture publisher also starts its encoder on demand.

Scenarios, each a transcode row in the [harness](/quest/m2/video-quality/harness.md):

- Steady live rung from the first frame, as the baseline.
- Subscribe mid-clip, unsubscribe, resubscribe.
- Fetch a run of past groups, then continue live. Grade fetch-to-fetch and
  fetch-to-live boundaries separately.
- The first GoP's VMAF after a cold start, against the steady state.

Fix candidates, to be chosen by the measurements:

- Keep one fetch encoder per rung across consecutive fetched groups, so rate
  control carries over the way it does for live.
- Seed a cold encoder's initial QP from the live rung's recent average,
  where the backend exposes it (NVENC `initialRCQP`).
- Tune the IDR quality offset, if the pop is I-frame pumping rather than a
  cold start.

Public API: none expected. Wire: none.

## Required

- [Harness](/quest/m2/video-quality/harness.md) - the scorer and pop metric

## Related

- [Ladder fetch](/quest/m3/ladder/fetch.md) - later makes fetch encode at the controller's target; keep the same fetch encoder ownership
- [Transcode rungs](/quest/m2/transcode-rungs.md) - may merge the live and fetch writers this quest touches
