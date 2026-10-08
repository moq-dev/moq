# [S] Lag across a splice

## Goal

A spliced subscription's `lag` histogram stays exact across a route switch:
every byte the viewer could read is weighed once, and nothing a retired
segment produces afterwards is weighed at all, however early in its life the
segment was replaced.

## Plan

Decided in the 2026-10-05 audit: this quest, planned in draft
[#4381](https://github.com/moq-dev/moq/pull/4381) against the QoS line branch,
lands as a quest on `main` now that questlines are flat, and the QoS line
([#4133](https://github.com/moq-dev/moq/pull/4133)) is held until it is
fixed. The sampler exists only on the line branch, so the fix PR targets
`quest/m1/qos/README` (or lands with #4133).

[#4298](https://github.com/moq-dev/moq/pull/4298) added the per-broadcast
viewer lag sampler (`FrontierInner::sample` in `rs/moq-net/src/stats.rs`) and
had a spliced subscription watch each segment's track
(`rs/moq-net/src/model/resume.rs`). Two Codex findings on its last round
merged unanswered, and the maintainer ruled in the 09-28 merged-PR audit that
both block the line:

- **Pending weight is lost when the replacement has no frame yet**
  ([r4113824520](https://github.com/moq-dev/moq/pull/4298#discussion_r4113824520)).
  `sample` takes `state.unsampled` before it knows a source supplies `newest`
  and `first`. When a capped segment folded bytes into `unsampled` and the
  replacement has not produced, the `?` returns and those bytes are gone.
  Keep the weight until a sample can record it, or keep the retired source's
  edge alongside its weight. Still present on the line branch on 2026-10-05.
- **A segment replaced before its first frame is never unwatched**
  ([r4113824522](https://github.com/moq-dev/moq/pull/4298#discussion_r4113824522)).
  `unwatch` runs only when an existing segment's `end` changes. A segment
  the producer replaces outright is marked `pruned` and dropped through
  `retired()` without it, so if its route keeps publishing, those
  unreachable bytes keep weighing this viewer's lag. Unwatch on every path
  that retires or replaces a segment, not just on a cap.

Extend the existing splice lag test (`lag_weighs_every_segment`) or add
siblings so each case fails without its fix. Rejected: two quests, since
both sit in the same splice accounting path and would conflict.

Final lag sample (#4451) changed the same sampler; keep any weight this
quest defers in its drop-time sample.

Public API: none. Wire: none, only the histogram's values change.
