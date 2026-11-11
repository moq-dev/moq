# [S] A scoped JS handle that closes reports its routes offline

## Goal

In `@moq/net`, routes a session received through a scoped `Producer` go
offline when that scoped handle closes while the parent origin stays open.
The parent's announcement reader emits `offline`, not `end` or `absent`,
unless the announcer ended the route on the wire.

## Plan

Planned 2026-11-10 (maintainer, /quest-plan on lite-07 routing). Milestone:
✅ m1 (rejected: m2 in the routes epic; m0).

Found by the automated review of
[moq-dev/moq#8375](https://github.com/moq-dev/moq/pull/8375), posted fifteen
minutes before it merged and never addressed. Closing the scoped handle runs
the raw callbacks `#hold` registered (`js/net/src/origin.ts`, about line
1695) and skips `close(lost)`, so received entries keep `lost = false` and
the reader reports a withdrawal. A later route close cannot repair it,
because `ended` is already true.

Reproduce it first in `js/net/src/origin.test.ts`: receive a route through a
scoped handle, close the handle, and read the parent's announcements. Then
put the reason in the shared release transition, so a received route's
teardown defaults to offline and only an explicit wire end selects
withdrawal.

Compare with Rust's scoped teardown and say in the PR whether it has the same
gap; if it does, fix it in the same PR, as `moq-net` and `js/net` stay in
step.

Caused by: [moq-dev/moq#8375](https://github.com/moq-dev/moq/pull/8375).

Public API: none. Wire: none.

## Related

