# [S] Demand

## Goal

The browser player subscribes to a catalog-referenced broadcast a wildcard
covers, instead of hiding renditions that are not announced. Without this, a
lazily-produced rendition is a deadlock: the encoder starts on demand, and the
player never demands what it hides.

## Plan

The gate is JS-only, and it is already half fixed.
[moq#3225](https://github.com/moq-dev/moq/pull/3225) made `#isPathAnnounced` in
`js/watch/src/broadcast.ts` prefix-aware: it holds the set of announced
prefixes and accepts any that covers the path, so a route at `room/` already
makes `room/alice/cam.hang` selectable without naming it. What it cannot do is
match a pattern, since it tests with `Path.hasPrefix`.

So the remaining work is narrow: teach the JS client the wildcard
advertisement (`js/net`'s announce handling, mirroring what
[advertise](/quest/m2/wildcard/advertise.md) does in moq-net) and make the
covering test use the shared pattern matching rather than prefix containment.
Withdrawal of the last covering wildcard hides the rendition again, the same
reactive shape announcements have today.

Do not simply delete the gate. It exists so the player does not subscribe to
absent broadcasts and so renditions appear and disappear reactively with
announcements. The Rust side needs nothing here: `moq-mux::Source` resolves
references through `request_broadcast`, which
[resolve](/quest/m2/wildcard/resolve.md) teaches to consult patterns.

Two existing soft spots to not reintroduce: the first evaluation runs before
the announcement stream has populated, briefly hiding cross-broadcast
renditions on startup; and a token without announce visibility over the
sibling's path hides it permanently even though a direct subscribe would work.
A covering wildcard fixes the second only if patterns are forwarded under the
subscriber's scope, which advertise's rebasing rule guarantees.

Tests: a rendition whose broadcast is covered only by a wildcard is listed and
playable, subscribing it is what starts production (the subscribe arrives
before any announcement), the rendition disappears when the last covering
wildcard is withdrawn, and a concrete announcement arriving later changes
nothing visibly.

## Required

- [Resolve](/quest/m2/wildcard/resolve.md) - recognizing the wildcard is useless
  until the relay routes the resulting subscribe through it
