---
title: moq-json
description: JSON over MoQ tracks, as snapshots, streams, or a sliding window
---

# moq-json

[![crates.io](https://img.shields.io/crates/v/moq-json)](https://crates.io/crates/moq-json)
[![docs.rs](https://docs.rs/moq-json/badge.svg)](https://docs.rs/moq-json)

JSON over [`moq-net`](/lib/rs/moq-net) tracks, in three modes:

- **snapshot**: lossy latest-value, with RFC 7396 merge-patch deltas.
- **stream**: lossless append-log in a single group.
- **window**: a bounded run of records a reader can join at any point.

On snapshot and stream, `Config` is the codec options. The track-owning pair is
`producer::Config` / `consumer::Config`. Compression is a shared `Compression`
enum (`None` or `Deflate`), not a bool: both sides set the same field. Window
still uses `ProducerConfig` / `ConsumerConfig` and a boolean `compression`
flag.

```rust
let mut config = moq_json::snapshot::Config::default();
config.compression = moq_json::Compression::Deflate;

let mut producer = moq_json::snapshot::Producer::new(track, config);
producer.update(&value)?;
```

A snapshot producer also edits in place, so independent owners each touch only
their own keys instead of clobbering one another. `mutate(|value| ...)` runs a
closure and publishes the result, matching `Producer.mutate` in TypeScript;
`modify()` returns a guard that holds the lock across several edits and
publishes on drop.

The TypeScript twin is [`@moq/json`](/lib/js/json). API:
[docs.rs/moq-json](https://docs.rs/moq-json).
