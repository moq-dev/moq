[![Documentation](https://docs.rs/moq-flate/badge.svg)](https://docs.rs/moq-flate/)
[![Crates.io](https://img.shields.io/crates/v/moq-flate.svg)](https://crates.io/crates/moq-flate)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-flate

Opaque byte tracks over [`moq-net`](https://docs.rs/moq-net), optionally
compressed with group-scoped DEFLATE, in two modes:

- **snapshot**: lossy latest value. A consumer gets only the most recent payload.
- **stream**: lossless append-log. Every payload is delivered in order.

The bytes are never inspected. Compression is opt-in per track: each group is
one raw DEFLATE stream, sync-flushed at every frame boundary, so later frames
reuse earlier ones as context. The bare `Encoder`/`Decoder` codec is exported
too, and [`moq-json`](https://docs.rs/moq-json) builds on it to add merge-patch
deltas for JSON documents. The TypeScript twin is
[`@moq/flate`](https://www.npmjs.com/package/@moq/flate).

```bash
cargo add moq-flate
```

See [doc.moq.dev](https://doc.moq.dev/lib/rs/moq-flate) and
[docs.rs/moq-flate](https://docs.rs/moq-flate).
