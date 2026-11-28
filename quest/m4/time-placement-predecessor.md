# [M] A time placement names the group before a pause

## Goal

A `Requests` handler answering a reader's time start that lands in a pause (after a group that ended whole before the time) finds the group before the pause with one upstream-placed lookup, never by reading the source's history. `moq-transcode`'s rungs use it, so a cold seek into a paused stream's history costs at most the one group that places it.

## Plan

`rs/moq-transcode/src/rung.rs` `newest` answers a time start in a pause by reading the span `[floor, time]` newest first for a candidate, then reading in key order from that candidate through the time. When this hop's copy of the source holds only an early group in the span and the true predecessor is still upstream, the candidate is that early group and the key-order read walks everything between it and the time. It is rare (fill floor 0, an early group cached here, the predecessor upstream) but unbounded in the source's history.

moq-net already places such a time: a reader's range waits until the publisher skips the pause, then starts at the predecessor's End (`Start::Time`, rs/moq-net/src/model/subscription.rs). Expose that answer, the predecessor's key or End, through public API on the subscriber (mirrored in `js/net`), and have `newest` look up that one group instead of reading for a candidate. Decide the API shape when building it; avoid adding a second lookup API next to `Subscriber::is_skipped` if one read can return both.

Found by the local review of [moq-dev/moq#9737](https://github.com/moq-dev/moq/pull/9737), accepted there as out of that PR's scope.
