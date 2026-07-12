<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/json

[![npm version](https://img.shields.io/npm/v/@moq/json)](https://www.npmjs.com/package/@moq/json)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

JSON publishing over [Media over QUIC](https://moq.dev/) tracks, in two modes:

- **`Snapshot`**: lossy. One JSON value updated over time; a consumer only gets the most recent value. Intermediate updates are collapsed and older groups are dropped.
- **`Stream`**: lossless. An ordered append-log of self-contained records; every record is preserved and delivered in order, nothing is ever superseded.

Pick `Snapshot` when consumers care about "what is the value now" (a catalog, a status document) and `Stream` when they care about every record (an event log, a media timeline).

## Quick Start

```bash
bun add @moq/json
```

### Snapshot: the latest value

A JSON value is published over a [`@moq/net`](../net) track as a series of groups. Each group is self-contained: its first frame is a full snapshot and any following frames are [RFC 7396 JSON Merge Patch](https://www.rfc-editor.org/rfc/rfc7396.html) deltas applied in order. A consumer jumps to the newest group, reads the snapshot, and applies the deltas, so a late joiner never needs older groups. This is lossy by design: only the most recent value is delivered.

```ts
import { Snapshot } from "@moq/json";

// Publish: each update supersedes the last.
const producer = new Snapshot.Producer(track);
producer.update({ hello: "world" });

// Consume: yields the latest reconstructed value, collapsing any backlog.
const consumer = new Snapshot.Consumer(track);
for await (const value of consumer) {
	console.log(value);
}
```

Pass `{ deltaRatio: x }` with a positive `x` to `Snapshot.Producer` to emit merge-patch deltas. A new snapshot group rolls once the deltas *already written* to the group exceed `x` times the fresh snapshot size; the delta that crosses the budget still lands first, so a group overshoots by at most one delta. `deltaRatio` defaults to `8` when unset. Set it to `0` to disable deltas: every change becomes a fresh single-frame snapshot group. Arrays are replaced wholesale within a delta; a value set to `null` falls back to a snapshot, since merge patch reads `null` as a key deletion.

### Stream: every record

An ordered log of self-contained records, one JSON value per frame, all riding a single group. Nothing is superseded: a consumer yields every record in order.

```ts
import { Stream } from "@moq/json";

const producer = new Stream.Producer(track);
producer.append({ event: "started" });
producer.append({ event: "stopped" });

const consumer = new Stream.Consumer(track);
for await (const record of consumer) {
	console.log(record);
}
```

Both modes support optional group-scoped DEFLATE compression (`{ compression: true }` on both sides), interoperable with the Rust `moq-json` crate.
