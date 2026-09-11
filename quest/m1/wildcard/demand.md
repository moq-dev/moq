# [S] Demand

## Goal

The browser player subscribes to a catalog-referenced broadcast a wildcard
covers, instead of hiding renditions that are not announced. Without this, a
lazily-produced rendition is a deadlock: the encoder starts on demand, and the
player never demands what it hides.

## Plan

The gate is JS-only, and it is already prefix-aware.
[moq#3225](https://github.com/moq-dev/moq/pull/3225) made `#isPathAnnounced`
(`js/watch/src/broadcast.ts:216`) hold the set of announced prefixes and accept
any that covers the path, so a route at `room/` already makes
`room/alice/cam.hang` selectable without naming it. What it cannot do is match
a pattern, since it tests with `Path.hasPrefix` (`:223`).

So the remaining work is narrow: teach the JS client the wildcard
advertisement (`js/net/src/announced.ts` and `js/net/src/lite/announce.ts`,
mirroring what [advertise](/quest/m1/wildcard/advertise.md) does in moq-net)
and make the covering test use `Path.Pattern` (`js/net/src/path.ts:526`)
rather than prefix containment.
Withdrawal of the last covering wildcard hides the rendition again, the same
reactive shape announcements have today.

Do not simply delete the gate. It exists so the player does not subscribe to
absent broadcasts and so renditions appear and disappear reactively with
announcements. The Rust side needs nothing here: `moq-mux::Source` resolves
references through `request_broadcast`, which
[resolve](/quest/m1/wildcard/resolve.md) teaches to consult patterns.

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

- [Resolve](/quest/m1/wildcard/resolve.md) - recognizing the wildcard is useless
  until the relay routes the resulting subscribe through it
