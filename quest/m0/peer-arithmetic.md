# [S] Peer and config values never panic or wrap moq-net arithmetic

## Goal

No value a peer sends or an application configures panics a moq-net task or silently wraps:

- `rs/moq-net/src/ietf/publisher.rs` `refusal`: a REQUEST_ERROR Retry Interval near `u64::MAX` panics through kio's `Instant + Duration`. Any peer we send PUBLISH_NAMESPACE to can trigger it.
- `rs/moq-net/src/ietf/publisher.rs` `save_join`: `largest.object + 1` on an upstream's LARGEST_OBJECT of `u64::MAX`.
- `rs/moq-net/src/reconnect.rs`: `now + duration` and `mul_f64` on `Goaway.handover`, `Config::timeout` and `Backoff` panic for huge configured values once a GOAWAY arrives; `Server::deadline` already treats overflow as no deadline.
- `rs/moq-net/src/lite/subscriber.rs` and `subscriber/fills.rs`: SUBSCRIBE_DROP's inclusive end `u64::MAX` becomes `..u64::MAX` through `saturating_add(1)`, so the drop misses group `u64::MAX`.
- `rs/moq-net/src/lite/publisher.rs` `DatagramSend::send`: a sequence of 2^62 or more relayed from lite-07 to a lite-05/06 subscriber fails to encode and is dropped silently, and the subscription ends in a reset instead of a finish.
- `rs/moq-net/src/route/mod.rs` `requests`: `fd[&id].0 + 1` on an advertised seqno `u64::MAX` (test-only today; becomes reachable when lite-07 drives the router).

## Plan

Found by the 2026-11-07 invariant audit (ietf-1, ietf-2, lite-3, lite-4, lite-9, model-3). Each site gets `checked_add` with an explicit meaning for overflow (no deadline, unsupported, refused) and a test at the extreme value. Prefer refusing the impossible value at decode or at `Reconnect::new` where that is simpler than handling it downstream.
