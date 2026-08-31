# [S] moq-wasm: bind track::Dynamic so a browser publisher can serve cache-miss fetches

## Goal

Implement and verify the behavior tracked in [#2835](https://github.com/moq-dev/moq/issues/2835)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Split out of #2814, where the Codex reviewer raised it.

\#2814 binds `TrackConsumer.fetchGroup(sequence, options)`, the *requesting* side of a FETCH. The serving side is missing.

When a browser publisher is asked for a group it no longer has cached, moq-net resolves the fetch only if the publisher holds a `moq_net::track::Dynamic` and answers its `requested_group()` with a `GroupRequest` (`accept(info)` / `reject(err)`). Otherwise the fetch fails with `NotFound`. `rs/moq-wasm` exposes neither, so from JavaScript every cache-miss fetch is unserveable.

Note the asymmetry this leaves: #2814 *does* bind the broadcast-level equivalent, `BroadcastProducer.requestedTrack()` over `broadcast::Dynamic`, so on-demand serving works for tracks but not for groups within a track.

Roughly what's needed:

- a `TrackProducer.requestedGroup()` entry point over `track::Dynamic`, minted lazily like `requestedTrack` does for `broadcast::Dynamic` (creating a `Dynamic` registers an on-demand handler, so it shouldn't happen for callers who never serve)
- a `GroupRequest` binding with `sequence`, `priority`, `accept(info)` -> `GroupProducer`, and `reject(code)`

Deliberately deferred out of #2814 rather than rushed in: it is new public surface, and everything else in that PR was verified in a browser against a relay. Serving a cache-miss fetch needs a harness that evicts or never caches a group and then fetches it, which is more than a review pass should bolt on.

Related: #2822 (datagrams, the other unbound part of the track model) and #2816 (no browser test harness, which is what would make verifying this cheap).

## Closes

- [#2835](https://github.com/moq-dev/moq/issues/2835) - close this issue when the quest finishes
