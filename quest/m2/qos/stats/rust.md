# [M] The Rust CLI, players, and encoders report stats

## Goal

`moq import --stats <path>` publishes a `.stats` broadcast reporting what the
import sends, `moq export --stats <path>` and `moq play --stats <path>` report
what the player received and played, and `moq export stats <path>` prints any
stats broadcast, the relay's `.stats/node/<node>` included, as JSON lines. A
publisher of a `.hang` broadcast can learn whether its viewers played it.

## Plan

- Counters at the source, in the crates that own the event: the `moq-video`
  and `moq-audio` encode producers count frames, bytes, keyframes, and drops;
  the decode and render paths count decoded, late, stalled, underruns, and
  errors, where `underruns` in `rs/moq-audio/src/playback` is already counted
  privately. Each exposes a `stats()` snapshot of cumulative counters; no
  callback, no per-frame lock.
- `moq-cli` attaches a `moq_net::stats::Session` to its client through
  `Client::with_stats`, folds the codec counters into `hang::Stats` per
  broadcast on the stats interval, and publishes through
  `moq_stats::Producer<hang::Stats>` in exact-path mode at the flag's path.
  A path that does not end in `.stats` is refused at parse time. The transport section comes from
  the connection's `ConnectionStats`.
- `moq export stats <path>` is a new sink: subscribe the stats broadcast and
  print each `publisher.json`, `subscriber.json`, and `sessions.json` frame
  as one JSON line tagged with its track, with `--track` selecting one and
  `--compressed` reading the `.z` twins. It is the one sink that does not go
  through `.hang` catalog discovery, so route it around `catalog_format`.
- `doc/bin/cli.md` documents the flag and the sink. The smoke media test
  publishes with `--stats`, plays with `--stats`, and reads both with
  `moq export stats`, asserting the subscriber's liveness advances and the
  publisher's frame count matches what was sent.

## Required

- [Schema and library](/quest/m2/qos/stats/schema.md) - the producer and the
  media types

## Closes

- [#2734](https://github.com/moq-dev/moq/issues/2734) - close this issue when the quest finishes
