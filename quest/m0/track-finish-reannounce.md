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

The question to settle is how a relay tells the two endings apart, and it has
to be settled before any of this is implemented, because re-arming alone
cannot answer it. `serve_track` calls `resume.finish()` on `Step::Complete`,
and `takeover` refuses a finished producer with `Error::Closed`. So a relay
that re-arms without a new signal has to pick one of two wrong things:
not finishing, which leaves a subscriber of a genuinely ended track waiting
forever, or replacing the map entry, which strands the subscribers holding the
finished consumer. Both boundaries are real, and today one signal serves both.

Two directions, once that is settled:

- Keep it inferred. The relay re-arms on a later lookup or demand, the way an
  aborted track already does, and something else has to carry the distinction:
  a linger, an announcement, or the route's own liveness. No wire change and no
  new public surface, at the cost of the relay guessing what the publisher
  meant.
- Make it explicit. The producer gets a third ending and `finish` stays
  terminal, so the publisher says which one it is. That is a public API
  addition across every binding, so it carries the Public API Scrutiny cost and
  likely a draft update, and it is the shape that makes the misuse
  unrepresentable rather than documented.

The reproduction comes first either way: it is what says whether the inferred
version can carry both boundaries at all.

Check `js/net` for the same asymmetry once the Rust behavior is settled.

## Related

- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - the other half of dynamic track identity across replacements
