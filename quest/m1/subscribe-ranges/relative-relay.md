# [S] Relative starts hold through a relay

## Goal

A `Relative(n)..` subscription through a lite-07 relay reads the same groups
as one over a single session: the n + 1 newest, not the oldest the relay
can reach.

## Plan

Found by the time-addressing spike
([moq-dev/moq@ebdfd55](https://github.com/moq-dev/moq/commit/ebdfd55fc606db8f2c990b937392b81291ab4715),
`spike_history`): with groups 0 to 3 published and a budget no group
outlives, `Relative(1)..` over one lite-07 session reads `[2, 3]`, but
through a cold relay (publisher, relay, client origins, two lite-07
sessions) it reads `[0, 1]`. `range_anchors.rs` covers only one session, so
extend its per-wire table with a relayed case, find where the relay's copy
resolves or forwards the anchor, and fix it there. Check `@moq/net` for the
same two-hop case.

Public API: none. Wire: none.
