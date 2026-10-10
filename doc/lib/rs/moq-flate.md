---
title: moq-flate
description: Opaque MoQ tracks, optionally compressed with group-scoped DEFLATE
---

# moq-flate

[![crates.io](https://img.shields.io/crates/v/moq-flate)](https://crates.io/crates/moq-flate)
[![docs.rs](https://docs.rs/moq-flate/badge.svg)](https://docs.rs/moq-flate)

Opaque payloads over [`moq-net`](/lib/rs/moq-net) tracks, in two modes:

- **snapshot**: lossy latest-value, one payload per group.
- **stream**: lossless append-log in a single group.

The bytes are never inspected. Compression is opt-in: `Compression` is `None`
(the default) or `Deflate`, and both sides set the same field. With `Deflate`,
each group is one raw DEFLATE stream sync-flushed at every frame boundary, so a
stream's payloads compress against the earlier ones in their group.

```rust
let mut config = moq_flate::snapshot::Config::default();
config.compression = moq_flate::Compression::Deflate;

let mut producer = moq_flate::snapshot::Producer::new(track, config);
producer.update(payload)?;
```

A payload is stamped when written, unless it carries its capture time:
`moq_net::Timed::from(bytes).at(captured)`. Writes return the encoded frame
size.

A stream rides one group, so the whole log shares moq-net's group budget:
32 MiB of payload and 8192 payloads. An `append` that might not fit returns
`moq_net::Error::GroupTooLarge` before it is encoded and leaves the log intact,
compressed or not. Once the budget is spent every `append` is refused; start a
new track to keep going. Any other failed append aborts the track, so readers
see the error rather than a log that looks complete.

The group-scoped codec underneath is exported as `Encoder`/`Decoder`, which
[`moq-json`](/lib/rs/moq-json) reuses for its merge-patch deltas. Create one
pair per group and feed frames in order.

The TypeScript twin is [`@moq/flate`](/lib/js/flate). API:
[docs.rs/moq-flate](https://docs.rs/moq-flate).
