# [S] IETF subscribers set their own max age

## Goal

A moq-transport subscriber bounds how late it is served with a SUBSCRIBE
and REQUEST_UPDATE parameter that carries moq-lite's subscriber max age
exactly: a group expires only once a newer group superseded it, judged on
media time and the hop's wall clock, so the newest group, such as a
long-lived catalog group, is never cut. Without it, the subscriber is served
as today.

## Plan

Decided 2026-10-19 by the maintainer (#5881):

- A new parameter, not the IETF delivery timeouts. OBJECT_DELIVERY_TIMEOUT
  resets a stream once an object has waited at the hop, newest group
  included: a late joiner's catalog snapshot would be reset and never
  delivered. SUBGROUP_DELIVERY_TIMEOUT runs from the FIN. Neither matches
  `max_delay`, so keep dropping both and say so in
  `doc/concept/standard.md`, replacing its "advisory" paragraph.
- Define it in `drafts/draft-lcurley-moq-timestamp.md`, whose age-based
  dropping already allows an application-supplied threshold. Pick a code
  point from that draft's space. Validate with `just drafts check`. Check
  each draft's rule for an unknown SUBSCRIBE parameter; where a peer may
  refuse one, send it only when negotiated.
- It maps onto `Subscription::max_delay` on SUBSCRIBE (`serving_subscription`
  in `rs/moq-net/src/ietf/publisher.rs`) and REQUEST_UPDATE. Absent keeps
  `MAX_SAFE_DELAY_MS`, never `Duration::ZERO`, which means skip at once.
- Both directions, as lite does: our IETF subscribers send their
  aggregated `max_delay` upstream, and the relay keeps lite's most-tolerant
  aggregation, so one aggressive viewer never tightens another's budget.
- Rust and `js/net`, both publisher and subscriber, at every IETF draft that
  carries SUBSCRIBE parameters. Test that a subscriber behind a shaped link
  with a short max age is skipped to the live edge, and that a catalog's
  newest group survives any budget.

Public API: none. Wire: a new SUBSCRIBE and REQUEST_UPDATE parameter.

## Closes

- [#5881](https://github.com/moq-dev/moq/issues/5881) - IETF subscribers cannot bound delivery lateness

## Related

- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - the upstream waiting budget, a different bound
- [Per-stream deadlines](/quest/m2/quic-deadline.md) - would derive per-stream resets from `max_delay`
