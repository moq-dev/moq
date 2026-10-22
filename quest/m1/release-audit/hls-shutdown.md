# [M] Release HLS server watchers with their owner

## Goal

Dropping the final HLS Server or router releases its background serving state
while independently owned responses and recordings retain what they need.

## Plan

P2 inherited lifecycle defect, confirmed at main 1d43c096e0. In
`rs/moq-hls/src/server/mod.rs`, each detached eviction task strongly owns
Inner and its Broadcaster until the source publisher closes. Inner owns the
broadcaster cache. Final Server drop cannot release that state, and main's
new whole-scope announcement watcher is retained with it.

A reproduction using real public Origin, Server, and Broadcaster APIs on a
single-thread runtime dropped all Server and external Broadcaster handles.
The broadcaster's weak handle still reported two strong references while
the publisher stayed live; closing the publisher reduced that to zero.
No wall-clock sleeps were used. This affects embedders replacing servers
while publishers remain live; it does not establish spontaneous growth in
one continuously used Server, and process exit masks the lifetime problem.

Make eviction work subordinate to the server's ownership. Preserve active
consumer ownership and the pointer-identity check that prevents an old task
from evicting a replacement broadcaster. Verify last-clone teardown with a
live publisher, active response ownership, and repeated replacement. No wire
change is expected; report any library lifetime/API change.
