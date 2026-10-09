# [S] A relay copy fails loud when upstream's position goes backwards

## Goal

A relay copy that subscribes upstream again and hears that upstream's largest
group is below the newest group it has cached treats it as a reused name: the
copy ends with an error, its source closes so the front ends, and the readers
it was serving re-request. A new request arriving as the front ends gets a
fresh front without an error. Today the copy keeps its stale live floor (`set_live` in
`rs/moq-net/src/model/track.rs`), so a viewer returning within the linger to a
publisher that restarted at group 0 gets the old instance's cached group and
then nothing until the new sequence passes it.

Covers lite-07 and moq-transport, whose answers carry the largest position.
lite-05 and 06 stay uncovered on purpose: their only hint is the first
group's sequence, and they keep waiting on the floor.

## Plan

Found while scoping idle fronts (2026-10-07):
under a prefix claim the relay never sees the worker close a broadcast, so a
restart at the same path reaches a lingering copy unannounced. A publisher
that restarts its group sequence under the same name is buggy, and this makes
that bug visible instead of a silent stall. A change of route can't cause
it: every route change, including a per-path winner change under a prefix
pool, reaches downstream as a `Restart` of the route, which unsets the cached
copy of every broadcast nested under it
(maintainer, 2026-10-08, in [Restart](/quest/m0/broadcast-epoch/restart.md)).
Only a restart behind an unchanged route reaches a copy unannounced.

Decided 2026-10-08: shrunk to the two answer-carrying wires and moved to m1.
The heuristic is unproven and risks a false failover, so it builds on the
front and route shapes Restart and idle fronts leave rather than ahead of
them.

Where it lands: since #4914, a rejoining copy goes idle (`set_idle`) and the
route's answer reaches `set_live`, which holds readers in `Feed::Answered`
until the cache shows the answered position. An answer below what the cache
already shows goes straight to `Feed::Live` and keeps the old floor; that is
the case to end instead, when it qualifies below.

A lower largest group alone doesn't prove a restart: a same-epoch resume onto
a replica that has fallen behind reports one legitimately, and must keep
waiting as today. The copy ends only when the answer comes from the route it
was last served from without an epoch, where no cross-route resume happens,
or names a different epoch than the copy holds. Check for any other
legitimate case before relying on that split.

Whatever ends a front, it leaves the origin's front table before its broadcast
closes or anything fails, and `request()` treats a front whose broadcast closed
as gone. So a reader re-requesting after seeing the front's end mints a fresh
front. The copy's error must reach readers through that end, not an `Abort`
ahead of it, or a re-request can still join the old front. A request that
joined before the front decided to end is in flight, like a subscription.

Verification: mocked time, a publisher restarting a path at group 0 within
the linger. The returning reader gets an error and then the new instance from
group 0, never the old group. A same-epoch standby behind the cached copy
keeps the copy and resumes it.

Public API: none. Wire: none.

## Required

- [Restart](/quest/m0/broadcast-epoch/restart.md) - route changes reach a copy as a `Restart`, leaving only same-route restarts for this to catch

## Related

- [Claim-served epochs](/quest/m1/claim-epochs.md) - prevents the splice on lite-07 by naming the instance
