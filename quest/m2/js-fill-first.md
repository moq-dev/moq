# [S] JS sends a group's fill ahead of its live rest

## Goal

When a draft-20 fill and the live stream of the same group are both in
flight, the JS publisher sends the fill (the earlier start object) first,
so a joining subscriber gets the group's start before its tail. Track
priority and group order still dominate. moq-lite ordering is unchanged.
This mirrors Rust's start-object tie-break from
[#5470](https://github.com/moq-dev/moq/pull/5470).

## Plan

Today `Priority#position` (`js/net/src/priority.ts`) assumes sequences are
unique within a subscription, but `#runFill` (`js/net/src/ietf/publisher.ts`)
registers a second stream for the live group's sequence, so the two tie.

Decided 2026-10-13 (maintainer): rank streams, not groups.
`Priority.add` takes the stream's start object, and `#position` also counts a
same-group stream with an earlier start as ahead. `sendOrder`'s packing is
unchanged, which avoids bounding the start object. Every stream passes the
object it starts at: an IETF group stream its first object, a fill its start,
and a Lite group its `frameRange` start (nonzero for a draft-06+
`startFrame`). Lite never registers two streams for one sequence, so its
order is unchanged.

Add a regression in `js/net/src/priority.test.ts` like Rust's
`a_fill_outranks_the_rest_of_its_group`: register the live rest first, then
the fill of the same group, and expect the fill to rank higher, in both group
orders, without the start ever overriding group order.

Public API: none (`Priority` is `@internal`). Wire: none.
