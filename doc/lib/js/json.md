---
title: "@moq/json"
description: JSON over MoQ tracks, as snapshots, streams, or a sliding window
---

# @moq/json

[![npm](https://img.shields.io/npm/v/@moq/json)](https://www.npmjs.com/package/@moq/json)

JSON over [`@moq/net`](/lib/js/net) tracks, in three modes:

- **Snapshot**: lossy latest-value, with RFC 7396 merge-patch deltas.
- **Stream**: lossless append-log in a single group.
- **Window**: a bounded run of records a reader can join at any point.

On Snapshot and Stream, `Config` is the codec options. `Producer.Config` /
`Consumer.Config` add the track. Compression is a shared `"none" | "deflate"`
enum, not a boolean: both sides set the same field. `Stream.Config` also
accepts a `schema` for record validation on encode and decode. A delta without
a snapshot raises `MissingSnapshot`; an uncommitted compressed stream frame
raises `Desync`. Unexpected frame read errors propagate to the caller. Window still uses
`ProducerConfig` / `ConsumerConfig` and a boolean `compression` flag.

```ts
import { Snapshot } from "@moq/json";

const producer = new Snapshot.Producer({ track, compression: "deflate" });
producer.update({ hello: "world" });

const consumer = new Snapshot.Consumer({ track: track.subscribe(), compression: "deflate" });
for await (const value of consumer) {
    console.log(value);
}
```

A value is stamped when written, unless you pass its capture time:
`producer.update(value, captured)`.

The Rust twin is [`moq-json`](/lib/rs/moq-json).
