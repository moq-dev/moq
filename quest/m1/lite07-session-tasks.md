# [M] A lite-07 session leaves no task behind

## Goal

Once a lite-07 session's handles and connection close, every task it spawned
ends, as lite-06's do. The session bench (`rs/moq-net/benches/session`)
asserts zero leftover tasks after each room drops, for both versions, so a
relay speaking lite-07 no longer accumulates one task per session it served.

## Plan

Planned 2026-11-10 (maintainer, /quest-complete follow-up of
[moq-dev/moq#8397](https://github.com/moq-dev/moq/pull/8397)). Milestone:
✅ m1 (rejected: m2 routes epic, m0, dropping it), since relays run lite-07
in production now.

#8397's `session_announce` bench found 18 tasks still alive in the
16-viewer delivery cell after a lite-07 room dropped, against 0 on lite-06:
about one per session. The per-cell runtimes from
[moq-dev/moq#8352](https://github.com/moq-dev/moq/pull/8352) hid this. The
likely cause is a lite-07 session driver (or a route-layer task it spawns)
that outlives its handles; confirm before fixing.

Reproduce on the bench first, then in a unit test on mock time. Fix the
owner of the leaked task at its root rather than aborting stragglers, and
keep the zero-task assertion in the bench for both versions.

Public API: none expected. Wire: none.
