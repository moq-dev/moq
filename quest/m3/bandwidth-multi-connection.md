# [L] One encoder follows several connections' estimates

## Goal

A track published over several connections at once, such as a studio
publishing to a primary and a backup relay, has its encoder follow every
connection's bandwidth estimate, so losing the primary does not leave it
holding the bitrate the primary last granted.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): m3, since the studio is a named consumer that is not urgent.

`bandwidth::Allocator::new` divides one estimate (`rs/moq-net/src/model/bandwidth.rs`,
"one estimate covers a whole connection"), and each allocator comes from one
connection (`moq_tokio::Connection::allocator`, `Session::allocator`). Nothing
combines them.

Consumer: studio failover ([moq-dev/studio#19](https://github.com/moq-dev/studio/pull/19)), which publishes to two
relays while its encoders follow only the first relay's estimate.
Recommendation, not settled: one reservation per connection, with the encoder
following the lowest grant among connections that still have readers, rather
than merging estimates into one allocator. Benchmark the fan-out over both
connection and track count.
