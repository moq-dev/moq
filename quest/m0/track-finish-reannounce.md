# [M] moq-net: a finished track's name is unreachable through a relay

## Goal

A publisher that finishes a track and later publishes the same name on the same
broadcast is reachable through a relay. Either a route replacement can take
over a finished logical track, or a publisher gets a way to end a track that
means "done for now" rather than "done forever".

## Plan

`broadcast::Consumer::track_inner` in `rs/moq-net/src/model/broadcast.rs` drops
a spliced logical track that is aborted, on the grounds that an abort is a
verdict from the sources attached at the time rather than a property of the
name, so the next request should reach a source again. A finished one is kept,
because its cache is still readable. There is no third state.

The consequence is that behind a relay, once a track finishes, every later
subscriber gets the finished cache. Nothing drains the pending queue, so the
route is never asked to serve that name again however many times upstream
re-creates it. Aborted is retryable, finished is terminal, and a publisher has
no way to say it is done for now.

This surfaced through `moq-transcode`: a resized ladder retires a rung by
finishing its track, and reusing the rung's name for the replacement looks
right and is unreachable, which is why #3381 gives every replacement a fresh
name. That works around it for one publisher. The asymmetry belongs to the
layer that owns it.

### Shape

Reproduce first, at the model layer rather than through `moq-transcode`: the
claim under test is that a second publication of a finished name is invisible
to a new subscriber through a relay.

Then decide the shape before writing the fix. Two candidates:

- Let a route replacement take over a finished spliced track the way
  `resume::Producer::takeover` already retains the live edge for an aborted
  one, so the cache stays readable while new groups land above it. No wire
  change, no new public surface.
- Give the producer an explicit third ending and keep `finish` terminal. That
  is a public API addition across every binding, so it carries the Public API
  Scrutiny cost and likely a draft update.

Prefer the first unless reproducing shows a case it cannot cover: a subscriber
holding the finished track has to see the continuation, and a publisher that
really is done forever must still be distinguishable.

Check `js/net` for the same asymmetry once the Rust behavior is settled.

## Related

- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - the other half of dynamic track identity across replacements
