# [L] SUBSCRIBE goes out with TRACK

## Goal

On moq-lite 05 and later, a subscriber opens its TRACK and SUBSCRIBE streams
together instead of waiting for TRACK_INFO before sending SUBSCRIBE, in Rust
and JS, at every hop: the viewer's session and each relay's upstream session.
First data arrives one round trip sooner per hop. A peer that still sends
them serially, and lite-03/04 with no track stream, keep working unchanged.

## Plan

Decided 2026-10-08 in a `/quest-plan` interview (paper trail in the PR that
added this quest). It starts after #5053 lands and reverses that PR's "not
pipelining" decision: the draft already allows it
(`drafts/draft-lcurley-moq-lite.md`, Track Stream: the subscriber "MAY open
the Track and Subscribe streams concurrently"), and the round trip is not
worth keeping.

Facts (2026-10-08, `origin/main` 4a79178d2):

- Rust: `TrackServeRun::poll` waits on `info.poll_fetch` before building its
  `ServeLoop` (`rs/moq-net/src/lite/subscriber.rs:3996`), and only then does
  `begin_subscription` and `Establish` write SUBSCRIBE.
- JS: `#openSubscribe` awaits `#trackInfo` before `Stream.open` and the
  SUBSCRIBE write (`js/net/src/lite/subscriber.ts:693`). The receive path
  already waits for the timescale (`runGroup`) and drops datagrams that beat
  TRACK_INFO.
- No SUBSCRIBE field needs TRACK_INFO, and neither publisher needs a TRACK
  before a SUBSCRIBE (`SubscribeServe::start` subscribes the model track
  directly).
- The relay front already hands demand to a copy before splicing it
  (`model/origin.rs` `Action::Query`), so the gap is in the session.
- Both stacks route groups only to an accepted subscription. Rust's
  `ServeLoop::new` accepts the request with the info, and
  `begin_subscription` registers a `TrackEntry` holding that producer and its
  timescale; a group whose id has no entry is cancelled, not left unread. JS
  `runGroup` drops a group for an id with no registered subscription, and
  writes the group before it waits on the timescale.

Decisions:

- Both streams open at once. Group streams that arrive before TRACK_INFO stay
  unread in QUIC until it lands (flow control bounds them; no buffering, no
  copies). Datagrams that beat it are dropped, as JS does today. Rejected:
  buffering decoded bytes in memory.
- Each subscription gets a pending state before accept, in both stacks:
  SUBSCRIBE is built from the request's subscription before the info is
  known, and its entry is registered under the id before SUBSCRIBE is written,
  with the producer and timescale deferred. A group stream that arrives early
  parks after its header and resumes on accept.
- A TRACK that fails or is reset while its SUBSCRIBE is live rejects the
  request, so the origin fails over as today, and resets the SUBSCRIBE
  stream. Rejected: keeping the subscription and learning the info another
  way.
- Only the max age SUBSCRIBE and SUBSCRIBE_UPDATE carry on the wire stops
  being clamped by TRACK_INFO (`max_age_bound()`, `model/track.rs`): it is the
  subscribers' own, and the publisher enforces its own. The local cache
  ceiling, the per-reader lateness budget, and the drift check keep
  `max_age_bound()`. Accepting TRACK_INFO never emits a SUBSCRIBE_UPDATE.
  Rejected: a SUBSCRIBE_UPDATE once TRACK_INFO lands. Narrowed 2026-10-08
  from review. This agrees with
  [One max_age meaning](/quest/m1/cache-max-age.md): both budgets apply one
  staleness rule, so a group is stale under the smaller budget exactly when it
  is stale under either.
- Publishers send a track's TRACK_INFO at a higher stream priority than that
  track's groups, in Rust and JS, as the draft's SHOULD asks, so the unread
  wait stays one round trip.
- Keep #5053's held TRACK stream: demand still has to hold across either
  arrival order and for serial peers.
- Legacy: mixed-mode integration tests in Rust and JS, a serial subscriber
  against a pipelining publisher and relay and the reverse, on lite-05/06/07,
  with lite-03/04 unchanged. `just test interop --all` covers the
  cross-language pairs.

Tests also: SUBSCRIBE is on the wire before TRACK_INFO; groups that arrive
first are read only after it; a reset TRACK rejects the request and the
origin fails over; accepting TRACK_INFO sends no SUBSCRIBE_UPDATE; TRACK_INFO
outranks the track's groups. Measure time to first frame across one and two
relay hops before and after.

Public API: none. Wire: none (ordering only; the draft already permits it).
Decided 2026-10-08: pipelining is ordering-only, so it neither waits on nor
blocks the Related quests below; whichever lands second rebases.

## Required

- [TRACK stream demand](/quest/m0/track-stream-demand.md) - the held TRACK stream this keeps (#5053)

## Related

- [Pipelined first FETCH](/quest/m1/pipeline-requests/fetch.md) - the same change for fetch-only readers
- [lite-07 Live flag](/quest/m1/lite-live.md) - reshapes the same SUBSCRIBE fields
- [One max_age meaning](/quest/m1/cache-max-age.md) - the staleness rule that makes the wire clamp redundant
- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - replaces the SUBSCRIBE floor with ranges
