# MoQ questline

## Goal

Keep the repository's living work organized as visible, versioned quests,
grouped by the branch they land on and ordered by priority.

## Plan

`main` and `dev` mirror those branches: work under either merges there,
directly or through its questline's branch. `next` and `future` are the
roadmap and have no branch; starting a quest moves it under the branch it
targets. Published API and wire breaks go under `dev`, everything else under
`main`. `dev` merges into `main` once its questline is empty.

## Quests

- [main](/quest/main/README.md) - work landing on main now: additive changes and the 0.0.x contracts
- [dev](/quest/dev/README.md) - published API and wire breaks, landed as one merge into main
- [next](/quest/next/README.md) - the next wave, in priority order
- [future](/quest/future/README.md) - later work, studies, and quests gated on the outside world
