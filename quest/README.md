# MoQ questline

## Goal

Keep the repository's living work organized as visible, versioned quests,
grouped into milestones ordered by priority.

## Plan

m1 is the dev branch line. m2 is the next agent
wave across reliability, features, performance, and planning. Unsettled
quests may stay in m2 for planning; their implementation waits for the
required decisions. m3 holds later features, design studies, experiments,
and hardware validation. Priority is separate from branch targeting:
published API breaks still target dev under the repository rules. An issue
already fixed on dev stays open until dev merges.

The 2026-09 audit keeps uring-TCP in m2 and defers catalog identity, mobile
ownership and dependent capture, Linux OBS GPU feasibility, the LiveKit
shim, and experimental QUIC probing. Independently useful binding, codec,
room, and transport work stays in m2.

## Quests

- [m1: the dev line](/quest/m1/README.md) - the breaking API and wire
  changes, the merge gates, and the merge itself
- [m2: next wave](/quest/m2/README.md) - implementation and planning across
  reliability, capabilities, and performance
- [m3: later work](/quest/m3/README.md) - deferred features, design studies,
  experiments, and hardware validation
