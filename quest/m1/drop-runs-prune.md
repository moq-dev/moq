# [S] Track drop runs are forgotten below the floor

## Goal

A track's record of dropped groups stays proportional to the window readers can still ask for, in Rust and JS. Today `drops` only grows: `RangeMap<u64, StreamError>` in `rs/moq-net/src/model/track.rs` is never pruned, and JS `insertDrop` (`js/net/src/internal.ts`) copies and re-merges the whole array for the state and every sink on each SUBSCRIBE_DROP, while `#drop` in `js/net/src/track.ts` scans the whole cache. A peer dropping alternating groups, or a long live track with sporadic gaps, grows it forever at quadratic total cost in JS.

## Plan

Found by the 2026-11-07 invariant audit (js-4). Runs below the cache/arrivals floor are forgotten, as `Delivered.forgetBelow` does, and JS inserts in place. Drop set ([moq-dev/moq#8476](https://github.com/moq-dev/moq/pull/8476)), which billed each reader's disposition record to the pool, left the track's own drop runs as they were; prune them in place.
