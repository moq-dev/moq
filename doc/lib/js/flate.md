---
title: "@moq/flate"
description: Opaque MoQ tracks, optionally compressed with group-scoped DEFLATE
---

# @moq/flate

[![npm](https://img.shields.io/npm/v/@moq/flate)](https://www.npmjs.com/package/@moq/flate)

Opaque payloads over [`@moq/net`](/lib/js/net) tracks, in two modes:

- **Snapshot**: lossy latest-value, one payload per group. `next()` yields every value in order; `latest()` skips to the newest.
- **Stream**: lossless append-log in a single group.

The bytes are never inspected. Compression is opt-in: `"none"` (the default)
or `"deflate"`, and both sides set the same field. With `"deflate"`, each
group is one raw DEFLATE stream sync-flushed at every frame boundary, the same
group-scoped DEFLATE `@moq/json` uses.

```ts
import { Snapshot } from "@moq/flate";
import { Time } from "@moq/net";

const producer = new Snapshot.Producer({ track, compression: "deflate" });
producer.update({ value: payload, at: Time.Timestamp.now() });
```

Producers take a `Timed` value, `{ value, at }`, where `at` is the capture
time written as the frame timestamp, and consumers return the same shape.
Nothing fills in now: a timed track, such as
`createTrack(name, { timescale: Time.Timescale.MILLI })`, needs `at` on every
write, and an untimed track (no `timescale`) takes none and reads back without
one.

A Stream rides one group, so the whole log shares `@moq/net`'s group budget:
32 MiB of payload and 8192 payloads. An `append` that might not fit throws
`GroupTooLarge` before it is encoded and leaves the log intact, compressed or
not. With `"deflate"` the check counts the raw payload plus DEFLATE's
worst-case overhead. Once the budget is spent every `append` throws; start a
new track to keep going. Any other failed append aborts the track, so readers
see the error rather than a log that looks complete.

The codec underneath is exported as `Encoder`/`Decoder`. Create one pair per
group and feed frames in order.

The Rust twin is [`moq-flate`](/lib/rs/moq-flate).
