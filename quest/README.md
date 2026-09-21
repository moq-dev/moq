# MoQ questline

## Goal

Keep the repository's living work organized as visible, versioned quests,
grouped into milestones ordered by priority.

## Plan

m0 contains the immediate priorities: the release API gates for the archive,
E2EE, socket, uring, and media packages, plus the Pronto desktop GPU path. m1
is the dev branch line. m2 is the next agent wave across reliability, features,
performance, and planning. Unsettled quests may stay in m2 for planning; their
implementation waits for the required decisions. m3 holds later features,
design studies, experiments, and hardware validation. m4 is deferred: work
whose first step is outside this repository. Priority is separate from branch
targeting: published API breaks still target dev under the repository rules. An
issue already fixed on dev stays open until dev merges.

The 2026-09 audit keeps uring-TCP in m2 and defers catalog identity, mobile
ownership and dependent capture, Linux OBS GPU feasibility, the LiveKit shim,
and experimental QUIC probing. Independently useful binding, codec, room, and
transport work stays in m2. The 2026-09-19 transport grooming settled one QUIC
stack (a moq-dev fork of noq), moved capacity probing back into m2, and opened
m4 for the hardware- and partner-gated quests.

## Quests

- [m0: immediate priorities](/quest/m0/README.md) - release API gates and the
  reusable Pronto GPU path
- [m1: the dev line](/quest/m1/README.md) - the breaking API and wire
  changes, the merge gates, and the merge itself
- [m2: next wave](/quest/m2/README.md) - implementation and planning across
  reliability, capabilities, and performance
- [m3: later work](/quest/m3/README.md) - deferred features, design studies,
  and experiments
- [m4: deferred](/quest/m4/README.md) - gated on the outside world: hardware
  nobody has, a partner, or a provider's offer
