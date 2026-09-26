---
title: "@moq/binary"
description: Opaque binary payloads over MoQ tracks
---

# @moq/binary

[![npm](https://img.shields.io/npm/v/@moq/binary)](https://www.npmjs.com/package/@moq/binary)

Opaque payloads over [`@moq/net`](/lib/js/net) tracks, in two modes:

- **Snapshot**: lossy latest-value, one payload per group.
- **Stream**: lossless append-log in a single group.

The bytes are never inspected. Compression is [`@moq/flate`](https://www.npmjs.com/package/@moq/flate),
the same group-scoped DEFLATE `@moq/json` uses.

`Config` is the codec options. `Producer.Config` / `Consumer.Config` add the
track. Compression is a shared `"none" | "deflate"` enum, not a boolean: both
sides set the same field.

```ts
import { Snapshot } from "@moq/binary";

const producer = new Snapshot.Producer({ track, compression: "deflate" });
producer.update(payload);
```

A payload is stamped when written, unless you pass its capture time:
`producer.update(payload, at)`.

The Rust twin is [`moq-binary`](/lib/rs/moq-binary).
