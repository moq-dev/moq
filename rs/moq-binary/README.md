[![Documentation](https://docs.rs/moq-binary/badge.svg)](https://docs.rs/moq-binary/)
[![Crates.io](https://img.shields.io/crates/v/moq-binary.svg)](https://crates.io/crates/moq-binary)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-binary

Opaque binary payloads over [`moq-net`](https://docs.rs/moq-net) tracks, in two
modes:

- **snapshot**: lossy latest value. A consumer gets only the most recent payload.
- **stream**: lossless append-log. Every payload is delivered in order.

The bytes are never inspected. Compression is
[`moq-flate`](https://docs.rs/moq-flate), the same group-scoped DEFLATE
[`moq-json`](https://docs.rs/moq-json) uses, which adds merge-patch deltas for
JSON documents. The TypeScript twin is
[`@moq/binary`](https://www.npmjs.com/package/@moq/binary).

```bash
cargo add moq-binary
```

See [doc.moq.dev](https://doc.moq.dev/lib/rs/moq-binary) and
[docs.rs/moq-binary](https://docs.rs/moq-binary).
