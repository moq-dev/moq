# [M] A route switch caps the old subscription at one boundary

## Goal

When a front moves a track to another route of the same instance, the reader
sees one subscription split at one boundary, chosen at the switch. The
boundary is the largest group any copy holds, past the frames held
contiguously in it. The new subscription starts there. The old one is capped
at that group on the wire at once and drains what lies below it within the
budget. Each frame is delivered once, from whichever copy brings it first.
Rust and `@moq/net` match.

## Plan

Found by the 2026-11-20 audit of the subscription rules before the lite-07
cut. Decided 2026-11-20 (maintainer, /quest-plan): ✅ cap the old at the
switch, by group (rejected: keep today's late bound; cap by frame now, which
[switch-frame-cap](/quest/m1/switch-frame-cap.md) adds on top of this).

Done in [moq-dev/moq#9276](https://github.com/moq-dev/moq/pull/9276):

- `Reader::sync` (Rust) and `sync` (`js/net/src/resume.ts`) pick the boundary
  G (`boundary`), start the new copy at (G, N), and cap every copy it replaces
  at G as a SUBSCRIBE_UPDATE at once (`Copy::cap`, `#cap`). Rust's `observe`
  and `bound_at_end` are gone. A group no replaced copy can drain is backfilled
  (`backfill_gaps`), and a replaced copy that fails hands its groups to one.
- With no boundary (nothing held nor handed out), the Rust reader drops the
  copies it replaces. `@moq/net` leaves them uncapped as incumbents, bounded
  at the new copy's first group (`#observe`).
- ResumeReader and FailoverChain model the boundary and pass every config.

What remains:

- Once the replacement delivers past the old copy's position in G, stop only
  G's stream on the old copy, which also ends a group that never finishes.
  Until then both routes send G's frames past N, and the reader takes each
  once.
- Teach the resume oracles (`resume_model.rs`, `resume.model.test.ts`) the
  fixed boundary. They check what each reader gets, which the boundary doesn't
  change, so this needs them to watch each route's wire subscription.

Test: a switch while the old route still delivers sends no group above G from
the old route, loses no group below G, and delivers G's frames once, in both
languages. The first two are covered; the stop of G is not.

Public API: none expected. Wire: none.

## Waiting

- Maintainer: where the stop of G goes. Options: here, per group in both
  languages; or in [switch-frame-cap](/quest/m1/switch-frame-cap.md), which
  caps the old copy at (G, N) and drops the stop step anyway. Recommended:
  switch-frame-cap, and this quest finishes once the oracle decision below
  settles.
- Maintainer: whether the resume oracles learn the boundary. Options: add a
  wire-subscription check to both oracles; or leave them on delivered content,
  with the boundary covered by the unit tests and both TLA+ models.
  Recommended: leave them.
- Maintainer: a switch with no boundary. Rust drops the replaced copies, and
  `@moq/net` keeps them as incumbents until the new copy delivers. Options:
  keep that split, since the JS incumbent stands in for the Rust front swapping
  only once a copy delivers; or make JS drop them too. Recommended: keep it.
- Maintainer: `CopiesBounded` in ResumeReader now allows one more backfill per
  route that failed while its copy still had groups to drain. Widening a
  backfill downward instead broke the `resume_model.rs` regressions.
  Recommended: accept the relaxation.

## Related

- [A replaced copy's short group continues from the replacement](/quest/m1/switch-partial-close.md) - the other gap the audit found at a switch
