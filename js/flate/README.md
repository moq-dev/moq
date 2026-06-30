<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/flate

[![npm version](https://img.shields.io/npm/v/@moq/flate)](https://www.npmjs.com/package/@moq/flate)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Group-scoped DEFLATE: a stream of self-delimited frames sharing one compression window.

A sequence of frame payloads is compressed into a single raw DEFLATE ([RFC 1951](https://www.rfc-editor.org/rfc/rfc1951.html)) stream, sync-flushed at each frame boundary. Every frame is self-delimited (byte-aligned, the window retained) while later frames reuse the earlier ones as context, so a stream of similar payloads (a snapshot followed by deltas, repeated records, log lines) compresses far better than each payload alone.

This is plain raw DEFLATE with a `Z_SYNC_FLUSH` after each frame, so it interoperates on the wire with any peer using the same primitive, including the Rust [`moq-flate`](https://crates.io/crates/moq-flate) crate. The fixed 4-byte sync-flush marker is stripped per frame ([RFC 7692](https://www.rfc-editor.org/rfc/rfc7692.html#section-7.2.1)'s permessage-deflate trick). There is no length prefix: the caller frames each slice ([`@moq/net`](../net) already does).

## Quick Start

```bash
npm add @moq/flate
```

```ts
import { Encoder, Decoder } from "@moq/flate";

const encoder = new Encoder();
const a = encoder.frame(new TextEncoder().encode("the quick brown fox"));
const b = encoder.frame(new TextEncoder().encode("the quick brown dog")); // smaller: reuses the window

// Feed slices to the decoder in the same order they were produced.
const decoder = new Decoder();
new TextDecoder().decode(decoder.frame(a)); // "the quick brown fox"
new TextDecoder().decode(decoder.frame(b)); // "the quick brown dog"
```

Create a fresh `Encoder`/`Decoder` pair per independent stream (in moq-net terms, per group). `new Encoder({ level })` sets the DEFLATE level (`0..=9`, default `6`); `new Decoder({ maxFrameSize })` caps how far a single frame may inflate (default 64 MiB), rejecting zip bombs.
