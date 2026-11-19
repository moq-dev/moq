# [M] A held group start is a value, not an error

## Goal

`moq_net::track::Producer::create_group` reports a start another live group
already holds as a value (`Result<Option<group::Producer>>`), as the fill
`accept`s do, not `Err(Error::Duplicate)`, so moq-transcode's rung and the
ietf subscriber's `open_fill_group` stop matching it. js/net's `createGroup`,
which throws `duplicate group`, returns `undefined` the same way.

## Plan

Found by [small-public](https://github.com/moq-dev/moq/pull/9150), which
reshaped `create_track` and the fill `accept`s but left `create_group` out of
its Goal. The matches to remove: `rs/moq-transcode/src/rung.rs` (the live
path and a fetch racing to write one group) and `rs/moq-net/src/ietf/subscriber.rs`
(`open_fill_group`, a widened fetch range refilling a group the copy owns).
Most callers create a fresh start and unwrap, so the migration is mostly
mechanical; the recommendation is `Result<Option<_>>`, matching the fill
`accept`s.

Public API: breaking in moq-net (`create_group`) and js/net (`createGroup`).
Wire: none.
