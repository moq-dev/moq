---
title: "@moq/flate"
description: Opaque MoQ tracks, optionally compressed with group-scoped DEFLATE
---

# @moq/flate

[![npm](https://img.shields.io/npm/v/@moq/flate)](https://www.npmjs.com/package/@moq/flate)

Opaque payloads over [`@moq/net`](/lib/js/net) tracks, in two modes:

- **Snapshot**: lossy latest-value, one payload per group.
- **Stream**: lossless append-log in a single group.

The bytes are never inspected. Compression is opt-in: `"none"` (the default)
or `"deflate"`, and both sides set the same field. With `"deflate"`, each
group is one raw DEFLATE stream sync-flushed at every frame boundary, the same
group-scoped DEFLATE `@moq/json` uses.

```ts
import { Snapshot } from "@moq/flate";

const producer = new Snapshot.Producer({ track, compression: "deflate" });
producer.update(payload);
```

A payload is stamped when written, unless you pass its capture time:
`producer.update(payload, at)`.

The codec underneath is exported as `Encoder`/`Decoder`. Create one pair per
group and feed frames in order.

The Rust twin is [`moq-flate`](/lib/rs/moq-flate).
