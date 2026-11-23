# [S] A track producer serves until nobody wants it

## Goal

`moq_net::track::Producer` has one method that runs a serving future until it
completes or the track's demand goes idle, then ends the track with
`abort_unwanted` and returns, so an app serving tracks on demand never writes
its own idle loop. A reader that joins while the track is ending keeps it: the
method waits again instead of dropping it.

## Plan

Apps that serve a track only while someone reads it repeat the same loop:
wait for `Demand::wanted_changed` to report idle, try `abort_unwanted`, and on
`Err(kept)` (a reader joined meanwhile) wait again on a fresh `Demand`, since a
stale one that already reported idle misses a reader that came and went. The
easy mistake is dropping the producer when demand goes idle, which races a
joining reader: it sees the track end under it. A downstream deployment hit
exactly that race as an end-to-end flake, and fixed it with a local helper
used in five places; this quest moves the helper here so it can delete its
copy at its next pin bump.

Shape, as the downstream helper has it: `async fn until_unwanted<T>(self,
serve: impl Future<Output = T>) -> Option<T>`, returning `Some` when `serve`
finishes first and `None` once the track ended unwanted (or demand errored).
Taking `self` makes a producer used after the abort unrepresentable.
Recommendation: a `Producer` method rather than a free function, beside
`abort_unwanted` and `demand()`, and mirrored in `moq-ffi` only when an FFI
consumer asks.

Test at `moq-net`'s model layer: a reader still in a group keeps the track
past its subscription's end, and the track ends once the last reader leaves;
a reader that joins between the idle report and the abort keeps it.
