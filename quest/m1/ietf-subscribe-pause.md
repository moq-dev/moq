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
([#6167](https://github.com/moq-dev/moq/pull/6167)): `Served::Paused` and the shared
`run_pausable` loop (`rs/moq-net/src/ietf/publisher/tracks.rs`). Pausing drops the
`track::Subscriber`, so its demand leaves the upstream aggregate, and resuming
subscribes again from the stored parameters at the live edge. Share that loop
between SUBSCRIBE and PUBLISH serves rather than duplicating it.

Dropping the `track::Subscriber` must not blind the paused state. The PUBLISH
paused wait watches only listing removal, session close, and the auth gate, so
a SUBSCRIBE paused there would never see its track end. Keep a watcher for the
track's end while paused, so a track that ends sends PUBLISH_DONE with
TRACK_ENDED, which the paused-SUBSCRIBE removal in
[outbound SUBSCRIBE_TRACKS](/quest/m1/archive/subscribe-tracks-outbound.md)
relies on. Carry the SUBSCRIBE's grant and pending verification across the
pause too. A
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

### Progress

[Candidate PR](https://github.com/moq-dev/moq/pull/6299) shares the PUBLISH
pause/resume loop with SUBSCRIBE, retains the request slot and renewal FIFO,
and closes every forwarding stream before acknowledging a pause. Initial
paused requests query metadata without subscribing to groups; the query is
dropped before SUBSCRIBE_OK. No Query or Subscriber is retained while paused.
The loop weakly observes the current serving source, so a locally produced
track can finish while paused without holding its delivery demand.

The model's logical front copy is not a suitable terminal observer: once its
last reader leaves, `origin` retires it with `Error::Cancel`. Observing the
source instead avoids that artificial local cancellation. Source cancellation
still ends the paused request with an error, never TRACK_ENDED.

### Decisions needed

The relay goal remains unfinished. The IETF subscriber cancels its upstream
SUBSCRIBE when delivery demand leaves, then retains the idle copy for the
existing 30-second `IDLE_LINGER`. Once no request wants it, `linger` retires it
with `abort_unused(Error::Cancel)`. The weak observer then ends the paused
downstream request too. Lite's subscriber also cancels unused copies. After
the upstream request is cancelled, no later remote TRACK_ENDED can arrive.
This candidate therefore fixes local pause/resume and renewal, but a remote
idle copy still causes delayed downstream termination.

Choose the ownership contract before completing this quest:

1. Recommended: define the requirement as "no upstream delivery subscription;
   a paused request may retain bounded metadata/end observation." Keep that
   observation within the existing request slot, and implement the upstream
   protocol behavior explicitly. This candidate does not retain such demand
   or change upstream cancellation.
2. Keep strictly zero upstream demand and hold the downstream request after
   its observer is cancelled. A later resume can resolve again, but remote
   completion is unobservable and cannot guarantee TRACK_ENDED.
3. Accept the candidate's explicit source-cancellation error. This retains
   zero demand and bounded state, but does not achieve persistent paused
   relay subscriptions or remove their resubscribe round trip.

Do not translate routing cancellation into a clean content end or silently
retain an answered Query to hide the distinction.

## Closes

- [#6263](https://github.com/moq-dev/moq/issues/6263) - Publisher ends the subscription on REQUEST_UPDATE with Forward=0

## Related

- [Outbound SUBSCRIBE_TRACKS](/quest/m1/archive/subscribe-tracks-outbound.md) - the subscriber side that sends Forward=0 and toggles it
