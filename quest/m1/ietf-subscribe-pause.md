# [M] Pause an IETF SUBSCRIBE

## Goal

moq-net's IETF publisher lets a subscriber pause and resume a SUBSCRIBE with
Forward State. REQUEST_UPDATE with Forward=0 stops delivery, answers
REQUEST_OK, and keeps the subscription established. A later Forward=1 resumes
it. An initial SUBSCRIBE with Forward=0 starts paused. While paused, the
subscription holds no upstream demand. A relay that pauses its upstream
subscription with no forwarding subscribers, as moqx does, no longer has its
subscription ended with UPDATE_FAILED. It also no longer pays an unsubscribe
and resubscribe round trip on each resume.

## Plan

Decided 2026-10-21 (maintainer): accept Forward=0 both on REQUEST_UPDATE and
on the initial SUBSCRIBE, as one paused state.

Reuse the pause machinery the SUBSCRIBE_TRACKS answer added for PUBLISH
(#6167): `TrackServe::pausable`, `Served::Paused`, and the paused wait in
`run_publish` (`rs/moq-net/src/ietf/publisher/tracks.rs`). Pausing drops the
`track::Subscriber`, so its demand leaves the upstream aggregate, and resuming
subscribes again from the stored parameters at the live edge. Share that loop
between SUBSCRIBE and PUBLISH serves rather than duplicating it. Today the
SUBSCRIBE serve maps `Served::Paused` to `Ok(())`
(`rs/moq-net/src/ietf/publisher.rs`), and `apply_subscription_update` and the
SUBSCRIBE admission refuse Forward=0.

Dropping the `track::Subscriber` must not blind the paused state. The PUBLISH
paused wait watches only listing removal, session close, and the auth gate, so
a SUBSCRIBE paused there would never see its track end. Keep a watcher for the
track's end while paused, so a track that ends sends PUBLISH_DONE with
TRACK_ENDED, which the paused-SUBSCRIBE removal in
[outbound SUBSCRIBE_TRACKS](/quest/m1/archive/subscribe-tracks-outbound.md)
relies on. Carry the SUBSCRIBE's grant across the pause too:
`apply_subscription_update` returns `Served::Paused` before it starts
`verify_renewal`, and the paused loop refuses AUTHORIZATION TOKEN. A
REQUEST_UPDATE with both Forward=0 and a token, or a token while paused, must
renew the grant rather than drop the token and later expire the request.

A paused SUBSCRIBE still occupies its request slot, so the per-session request
caps bound how many a peer can hold. LOCATION_FILTER and FILL_PARAMETERS on a
SUBSCRIBE update stay refused; only the forward state and the parameters the
shared loop already accepts change.

Replace `unsupported_subscription_update_fails_the_subscription`'s Forward=0
case and its wire test with pause and resume tests: a paused SUBSCRIBE drops
upstream demand, a track ending while paused sends TRACK_ENDED, and a combined
pause-and-renew update keeps the request authorized past the old grant. Update `doc/concept/standard.md`, which
documents the Forward=0 refusal. Run `just test interop --all`.

Public API: none. Wire: Forward=0 on SUBSCRIBE and on its REQUEST_UPDATE is
served instead of refused.

## Closes

- [#6263](https://github.com/moq-dev/moq/issues/6263) - Publisher ends the subscription on REQUEST_UPDATE with Forward=0

## Related

- [Outbound SUBSCRIBE_TRACKS](/quest/m1/archive/subscribe-tracks-outbound.md) - the subscriber side that sends Forward=0 and toggles it
