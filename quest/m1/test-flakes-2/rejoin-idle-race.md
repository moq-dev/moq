# [S] Rejoin after the copy goes idle

## Goal

A reader rejoining a parked IETF track never gets the stale warm cache
first, and moq-tokio `broadcast_rejoin_skips_a_stale_warm_cache`
(`rs/moq-tokio/tests/broadcast.rs`) passes in every full-suite run.

## Plan

It failed once under a loaded `just check` ("moq-transport-17: a rejoining
reader was served the stale cache first: group 3") and passed 8/8 alone.

Likely cause: on idle, the IETF subscriber awaits `cancel_subscribe` before
it calls `track.set_idle()` (`rs/moq-net/src/ietf/subscriber.rs`, around
line 2178 at time of writing). Draft-17 has no UNSUBSCRIBE, so the cancel is
a STOP_SENDING followed by a `close().await` that waits for the peer.
The publisher sees the cancel and its demand drops, which is what the test
waits on, about a round trip before the copy is marked idle. A resubscribe
in that gap finds a copy that still looks live. Its newest group, 3, has no
successor, so it isn't stale, and it is served.

Decided 2026-10-05 11:39 +0200: fix the product, since a real client
rejoining in that gap gets the stale group too. Reproduce the race first,
then mark the copy idle as soon as `End::Idle` is decided, before the
awaited cancel. The test then waits on an event that comes after idle and
needs no change. Rejected: only making the test wait for the subscriber's
idle, which leaves the window open for clients. Check the other drafts and
lite for the same ordering. Add a deterministic regression test if one is
easy.

Public API: none. Wire: none.
