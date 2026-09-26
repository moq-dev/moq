---
title: moq-binary
description: Opaque binary payloads over MoQ tracks
---

# moq-binary

[![crates.io](https://img.shields.io/crates/v/moq-binary)](https://crates.io/crates/moq-binary)
[![docs.rs](https://docs.rs/moq-binary/badge.svg)](https://docs.rs/moq-binary)

Opaque payloads over [`moq-net`](/lib/rs/moq-net) tracks, in two modes:

- **snapshot**: lossy latest-value, one payload per group.
- **stream**: lossless append-log in a single group.

The bytes are never inspected. Compression is [`moq-flate`](https://docs.rs/moq-flate),
the same group-scoped DEFLATE `moq-json` uses.

`Config` is the codec options. The track-owning pair is `producer::Config` /
`consumer::Config`. Compression is a shared `Compression` enum (`None` or
`Deflate`), not a bool: both sides set the same field.

```rust
let mut config = moq_binary::snapshot::Config::default();
config.compression = moq_binary::Compression::Deflate;

let mut producer = moq_binary::snapshot::Producer::new(track, config);
producer.update(payload)?;
```

A payload is stamped when written, unless it carries its capture time:
`moq_net::Timed::from(bytes).at(captured)`. Writes return the encoded frame
size.

The TypeScript twin is [`@moq/binary`](/lib/js/binary). API:
[docs.rs/moq-binary](https://docs.rs/moq-binary).
