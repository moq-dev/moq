# [L] TRACK_STATUS is answered without attaching a route

## Goal

A relay answers TRACK_STATUS for a broadcast it does not currently serve by
asking upstream rather than by subscribing. When no subscription is
outstanding for the track, the request is forwarded toward the origin as a
status request and the reply relayed back; when one is, the relay answers
from the live edge it already holds. No route is attached and no media is
pulled for a request that delivers none.

## Plan

- The IETF wire already has the request on every draft. moq-lite has no
  status request, so a relay whose upstream speaks lite needs one: specify a
  lite message that asks for a track's largest location and status without a
  subscription, in `drafts/draft-lcurley-moq-lite.md`, mirrored in js/net.
  Whether it reuses TRACK_INFO's payload is the first question.
- Routing: resolve the best server for the path as `request_broadcast` does,
  but send the status request over that session instead of a subscribe, with
  the same authorization the subscribe would have needed. A wildcard
  advertiser may answer for a path it has not started.
- Measure: a status request against an unserved broadcast attaches no route
  and moves no groups; a request against a served one is answered locally.
- This is the follow-up to the local answer the publisher already gives, which
  subscribes to read the live edge; a written verdict that the forwarding is
  not worth a lite message is a valid outcome.

## Related

- [Wildcard](/quest/m2/wildcard/README.md) - a covering advertiser is who answers for an unstarted path
