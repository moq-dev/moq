# [M] Demand holds across a subscription's TRACK and SUBSCRIBE

## Goal

On moq-lite 05 and later, a publisher sees one `used` edge when a viewer
subscribes, not `used`, `unused`, `used`. Today demand drops for one round
trip per hop between the TRACK request and the SUBSCRIBE that follows it. A
publisher that stops work on `broadcast::Demand::unused`, which is what
`Demand` documents itself for, closes its output on every first viewer and is
asked to create it again a few milliseconds later; a transcoder tears down its
encoders and waits for the next keyframe. It happens under a prefix claim and
for an exact announce alike. lite-04 and moq-transport don't do this. Rust,
JS, and the lite draft.

## Plan

Facts from `main`:

- The publisher answers TRACK with a track `query()`, which counts as demand
  on the track and its broadcast, and drops it once TRACK_INFO is written
  (`lite/publisher.rs`, `TrackInfoServe`). At the publisher's front it
  arrives as an `Asked` with no subscription to hold; the `Asked::sub`
  comment covers moq-transport's case, not this one.
- The subscriber sends SUBSCRIBE only after reading TRACK_INFO. At a relay,
  the upstream SUBSCRIBE waits for the downstream reader's own SUBSCRIBE, so
  the gap grows by a round trip per hop. JS subscribers also await TRACK_INFO
  before SUBSCRIBE.
- A relay's own copy flaps too, but `track::IDLE_LINGER` hides that from
  everything downstream. Only an end publisher sees the raw edge.

Decisions (2026-10-07):

- An open TRACK stream counts as interest. The publisher keeps its query
  until the requester FINs or resets the TRACK stream. The subscriber FINs it
  once its SUBSCRIBE stream has its first response (the start, a group, an
  end, or a reset), or once its own demand goes (an info-only query). The hold
  chains across hops, so demand stays continuous end to end. It is backward
  compatible: an older subscriber FINs at once, as today.
- Holding only until SUBSCRIBE is sent isn't enough: QUIC doesn't order the
  two streams, and a relay that handles the TRACK FIN before the SUBSCRIBE
  parks the track and drops its copy at once, letting go upstream. Waiting for
  the first response costs one stream held for about a round trip.
- A held TRACK stream is demand without a subscription, so it counts against
  the per-session subscription cap from
  [Request caps](/quest/m0/request-caps.md); a peer can't hold more interest
  than it could by subscribing. A held TRACK and the SUBSCRIBE for the same
  track on the same session share one slot: the SUBSCRIBE takes over the
  TRACK's reservation instead of needing a second, so a session at the cap
  can still turn its held TRACKs into subscriptions (decided 2026-10-08 from
  review).
- Not pipelining TRACK and SUBSCRIBE, which the draft already allows: it
  still races, and every hop would have to buffer frames until TRACK_INFO.
- Consumers never debounce demand; the docs promise clean edges.
- Scope: Rust and JS, plus one sentence in the draft's Track Stream section.
  Run `just test interop --all`.

Verification: a `moq-net` integration test on the simulated network (10 ms
latency) asserting exactly one `used` edge and no `unused` while the reader
stays subscribed, on lite-05, 06, and 07, direct and through one relay. It
fails on `main` today. The relay case controls the ordering so the
downstream TRACK FIN is handled before its SUBSCRIBE. A boundary test fills
the per-session subscription cap with held TRACK streams, turns each into a
live SUBSCRIBE with no `unused` edge, and checks that one more TRACK is
refused. JS counterparts for the JS side.

Public API: none. Wire: semantics only (holding the TRACK stream open), no new
fields.

## Required

- [Request caps](/quest/m0/request-caps.md) - the per-session subscription cap a held TRACK stream counts against

## Related

- [Idle fronts](/quest/m0/idle-fronts.md) - found in the same transcode-pool report
- [#4225](https://github.com/moq-dev/moq/pull/4225) - holds a lite subscription's demand in the same publisher code; expect a conflict
