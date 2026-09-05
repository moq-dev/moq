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

This is specific to the spliced logical tracks a route-fed broadcast mints. On
the plain path a few lines below, the weak cache drops any closed entry,
finished or aborted alike, and the request falls through to a source again. So
the same publisher is reachable in process and unreachable one relay hop away:
once the spliced track finishes, every later subscriber gets the finished
cache, nothing drains the pending queue, and the route is never asked to serve
that name again however many times upstream re-creates it. Aborted is
retryable, finished is terminal, and a publisher has no way to say it is done
for now.

This surfaced through `moq-transcode`: a resized ladder retires a rung by
finishing its track, and reusing the rung's name for the replacement looks
right and is unreachable, which is why #3381 gives every replacement a fresh
name. That works around it for one publisher. The asymmetry belongs to the
layer that owns it.

### Shape

Reproduce first, at the model layer rather than through `moq-transcode`: the
claim under test is that a second publication of a finished name is invisible
to a new subscriber through a relay.

Note that route replacement is only half the problem, and the smaller half.
When the same upstream broadcast stays connected and later serves the name
again, no replacement happens at all: `origin::serve_track` already returned on
its `Step::Complete`, and `track_inner` keeps handing back the finished entry
without queuing anything. So whatever lands has to re-arm serving on a later
lookup or demand, not just splice a new route in.

Then decide the shape before writing the fix. Two candidates:

- Re-arm the spliced track: keep the finished cache readable, but let a later
  lookup queue the name again, the way an aborted one already does, and let a
  source (the same one or a replacement) take over the way
  `resume::Producer::takeover` retains the live edge. No wire change, no new
  public surface. The work is in `serve_track` returning too early as much as
  in `track_inner`.
- Give the producer an explicit third ending and keep `finish` terminal. That
  is a public API addition across every binding, so it carries the Public API
  Scrutiny cost and likely a draft update. It also makes the re-arm
  publisher-driven rather than something the relay infers.

Prefer the first unless reproducing shows a case it cannot cover: a subscriber
holding the finished track has to see the continuation, and a publisher that
really is done forever must still be distinguishable.

Check `js/net` for the same asymmetry once the Rust behavior is settled.

## Related

- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - the other half of dynamic track identity across replacements
