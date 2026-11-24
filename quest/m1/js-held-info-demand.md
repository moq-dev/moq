# [S] A held JS track info lookup counts as its track's demand

## Goal

In `js/net`, a track info lookup held open (`track.Consumer.info({ signal })`,
or a relay's held downstream TRACK) keeps its track wanted after the track is
accepted, as Rust's `track::Resolving` counts as the track's `Demand` while
held, answered or not. A JS relay holding a downstream TRACK then keeps its
copy, and that copy's upstream subscription, until the TRACK closes.

## Plan

Found in the PR for the deleted quest `js-relay-track-from-copy`: a JS relay
now answers a downstream TRACK from its accepted copy's info, holding nothing
upstream, while a held lookup only keeps a track wanted before it is accepted
(`hooks.requestWanted` folds `Lookups.pending` into the request's demand).
After acceptance, `track.Producer`'s demand counts only subscribers
(`#reportWanted`: `this.#sinks.size > 0`), so the copy's subscription closes
once its last reader leaves, even while a downstream TRACK still holds it.
The downstream SUBSCRIBE that follows its TRACK then asks upstream afresh:
the same answer, one round trip slower.

- Count a held lookup in the producer's demand for as long as it is held,
  before and after acceptance, so `Request.demand()`, the lite subscriber's
  serve loop (`unwanted(demand)`), and on-demand publishers all see it.
- The lite `ConsumeBroadcast.resolveTrackInfo` answering from a held copy
  registers its hold the same way.
- Test: a JS relay whose copy's last reader leaves while a downstream TRACK
  holds the track keeps its upstream subscription until the TRACK closes.

Public API: none. Wire: none.
