# [S] Join moq-audio at the latest group without narrowing the wire budget

## Goal

`moq-audio`'s decode consumer joining with `Start::Latest` and a catalog warmup loses no
group a publisher bursts before its first frame is decoded.

## Plan

It subscribes with a budget of just the warmup and widens it to `max_delay + warmup`
after the first frame (`joining_latest` in `rs/moq-audio/src/decode/consumer.rs`). Until
the widening reaches the publisher, a group whose reach falls a warmup behind the newest
frame is skipped upstream, and no later update recovers it. The TS export had the same
shape and lost MPEG-1 audio frames right after its join; moq-mux's
`a_live_start_loses_no_group_the_publisher_bursts` shows the mechanism over an in-memory
session.

Reproduce it the same way first: a publisher bursting one-frame groups over
`moq_net::transport::memory` before the update lands. If it holds, subscribe with the full
budget from the start and keep the warmup reach local, as the export now picks its live
edge in the consumer instead of on the wire.

