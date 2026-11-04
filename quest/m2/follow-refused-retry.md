# [S] A refused follower asks a replacement handler

## Goal

A `Following` whose last resolution was refused asks again once the
handler that refused it is replaced, even when the replacement registers
the same instance with identical route details. Announcement listeners
still see no change for such a swap.

## Plan

Planned 2026-11-04 with the maintainer, from a review thread declined on
[moq-dev/moq#7836](https://github.com/moq-dev/moq/pull/7836)
([`follow.rs`](https://github.com/moq-dev/moq/pull/7836#discussion_r4471135777)).
Milestone m2 (maintainer).

The gap: since #7836, a refused `Following` (its `refused` flag in
`rs/moq-net/src/model/follow.rs`) waits for an announcement change before
asking again: another instance, the path going and coming back, or the
same instance over another route. `TableCursor::update`
(`rs/moq-net/src/model/origin.rs`, the `previous.meta == meta &&
previous.served == served` arm) deliberately delivers nothing when the
winning route keeps its instance, prefix, servability and metadata. So a
handler dropped and re-registered with an identical route stays refused
until something else changes. Making that swap an announcement event was
declined: it would change announcement semantics for every listener, not
just refused followers.

Recommended: after a refusal, the follower also waits on the route table's
path-scoped `Watch` (crate-private, in `origin.rs`), the generation
`routed_broadcast` already waits on after an `Unroutable`. It moves on any
change to the routes covering the path, including a handler swap, and
wakes only what watches that path, so listeners see nothing new. Check that
re-registering an identical route does bump it; if it doesn't, bump it
there, not in the announcement. Not chosen: a timed retry, which #7836's
decision 2 (maintainer, 2026-11-04) ruled out.

Fold this into whatever `follow` becomes under
[Listing and follow](/quest/m1/api-audit/route/events.md), which moves
instance changes inside `follow`. If the path watch changes under
[front parking](/quest/m2/origin-front-parks.md), use its replacement.

Tests: a refused follower asks the replacement after an identical-route
handler swap; an announcement listener sees no event for that swap; and a
follower that nothing replaced still doesn't ask the refusing handler again.

Public API: none. Wire: none.

## Required

- [HLS lists a broadcast's contributions without starting their media](/quest/m0/hls-contributions.md) - its PR, [moq-dev/moq#7836](https://github.com/moq-dev/moq/pull/7836), adds the refused follower this extends

## Related

- [Listing and follow](/quest/m1/api-audit/route/events.md) - `follow` takes over instance changes from announce events
- [Front parking](/quest/m2/origin-front-parks.md) - the same refusal wait for `routed_broadcast`
- [The HLS server follows each path once](/quest/m2/hls-follow-once.md) - another `Following` addition
