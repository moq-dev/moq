# MoQ questline

## Goal

Keep the repository's living work organized as visible, versioned quests,
grouped into milestones ordered by priority.

## Plan

Milestones sort by kind, which tracks urgency here: m0 fixes what is broken,
m1 is the dev branch line, m2 grows the surface, m3 explores. Every open
GitHub issue that survived the 2026-08 grooming is represented by a quest, and
the upstream-facing questlines from the downstream moq.pro tree were migrated
in. New work joins the milestone matching its kind, at its priority rank.

## Quests

- [m0: bug fixes](/quest/m0/README.md) - defects in what main ships today,
  security first
- [m1: the dev line](/quest/m1/README.md) - the thread-per-core runtime, net
  model follow-ups, breaking bindings work, and the archive line that gates
  the dev merge
- [m2: features](/quest/m2/README.md) - new capabilities on stable surfaces,
  from wire extensions to E2EE to developer packages
- [m3: prototypes](/quest/m3/README.md) - experiments, spikes, hardware
  validation, and measured go/no-go verdicts
