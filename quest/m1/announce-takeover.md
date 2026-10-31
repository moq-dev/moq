# [M] Announcements take over a claim's existing instances

## Goal

A worker advertises an epochless prefix to accept lazy work, then announces
an independent, more-specific route with a unique job epoch without resetting
readers already served by that same job. A different worker or replacement
job never inherits those readers. Rust and JS expose the transition as
`Takeover`, distinct from `Restart`.

## Plan

Rust, the lite-07 wire (ANNOUNCE_TAKEOVER, 0x5), the lite draft, the concept
docs, and the FFI bindings landed in
[moq-dev/moq#6994](https://github.com/moq-dev/moq/pull/6994): the origin
records which incarnation a route continues at install, fronts and session
selections follow each takeover as it lands (so a chain of them too),
cursors deliver `Takeover` only where every route they saw at the
predecessor's prefix serves that incarnation, and relays forward it on that
same condition, falling back to Start or Restart. The identity guide
describes it.
Keep the rest in m1 as a prerequisite of the lite-07 cut. moq-transport gets
no counterpart, since [moq-transport carries no epoch](/quest/m1/ietf-epoch-cut.md)
(decided 2026-10-28), so a moq-transport hop uses Start/Restart.

Decided 2026-10-29 (maintainer, settling #6994): the event stays
`Takeover { announce, predecessor }`, with `predecessor == announce.prefix`
in place; the C ABI's `moq_announce_event` carries the predecessor fields
rather than gaining an accessor; and a reader whose cursor saw a takeover
withdrawn before delivery gets the claim's later answers as Start/Restart.
A chain of in-place takeovers landing before a reader looks again is fixed
in that PR, without keeping history of retired routes.
JS continuity and epoch-bound requests through an adopted source moved here.

What remains:

- JS continuity. `js/net` decodes ANNOUNCE_TAKEOVER and keeps the Announce
  IDs in step, but surfaces it as a `start` (or a `restart` at the
  predecessor's own prefix), as an older wire would. Add a `takeover` kind
  carrying the predecessor's prefix to `announced.Event`, and make the lite
  subscriber's `BroadcastCache` re-key a consumer resolved through the
  predecessor's current advertisement, beneath the new prefix, under the new
  route instead of invalidating it. Handle the kind in
  `connection/forward.ts` and any player that follows announcements, so a
  `@moq/watch` reader through a claim keeps playing.
- Epoch-bound requests through an adopted source. A Rust front or selection
  that adopts a takeover keeps the epochless upstream SUBSCRIBE it already
  sent; a later request naming the new epoch joins it without restamping.
  This is accepted for now, since the answering worker is the instance by
  construction. Decide whether that join needs the upstream's answer to
  confirm the epoch.
- Tests not yet written, with controlled time: a SUBSCRIBE or FETCH pending
  at the front between its resolve and the takeover (`PendingFront::via`),
  a takeover naming a predecessor already restarted (the `seen` stamps in
  `OriginConsumerState`), conflicting epochs, a diamond of
  relays, a claim ending before or after its child, and held catalog pins
  through a real player rather than route-table checks.

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - the route layer whose ANNOUNCE_START layout overlaps this one's
