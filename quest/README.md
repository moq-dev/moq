# MoQ questline

## Goal

Keep the repository's living work organized as visible, versioned quests,
grouped into milestones ordered by priority.

## Plan

Milestones sort by kind, which tracks urgency here: m0 blocks the release,
m1 is the dev branch line, m2 grows the surface, m3 explores. The 2026-08
grooming turned every surviving GitHub issue into a quest and migrated the
upstream-facing questlines from the downstream moq.pro tree; issues opened
since are imported in periodic grooming passes (the last one on 2026-09-05),
and an issue already fixed on `dev` stays open until `dev` merges. The
2026-09-12 pass narrowed m0 to the release blockers and moved every additive
dev quest to m2. New work joins the milestone matching its kind, at its
priority rank.

## Quests

- [m0: release blockers](/quest/m0/README.md) - what must land before dev
  merges and the release is cut
- [m1: the dev line](/quest/m1/README.md) - the breaking API and wire
  changes, the merge gates, and the merge itself
- [m2: features](/quest/m2/README.md) - new capabilities on stable surfaces,
  from wire extensions to E2EE to developer packages
- [m3: prototypes](/quest/m3/README.md) - experiments, spikes, hardware
  validation, and measured go/no-go verdicts
