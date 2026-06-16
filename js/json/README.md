<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/json

[![npm version](https://img.shields.io/npm/v/@moq/json)](https://www.npmjs.com/package/@moq/json)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Snapshot/delta JSON publishing over [Media over QUIC](https://moq.dev/) tracks using [RFC 7396 JSON Merge Patch](https://www.rfc-editor.org/rfc/rfc7396.html).

A JSON value is published over a [`@moq/net`](../net) track as a series of groups. Each group is self-contained: its first frame is a full snapshot and any following frames are JSON Merge Patch deltas applied in order. A consumer jumps to the newest group, reads the snapshot, and applies the deltas, so a late joiner never needs older groups.

Deltas are opt-in via `maxDeltaRatio` (omit it to publish a full snapshot per group).

## Quick Start

```bash
npm add @moq/json
```

```ts
import { Producer, Consumer } from "@moq/json";

// Publish: deltas off by default (one snapshot per group).
const producer = new Producer(track);
producer.update({ hello: "world" });

// Consume: yields the reconstructed value after each update.
const consumer = new Consumer(track);
for await (const value of consumer) {
	console.log(value);
}
```

Pass `{ deltaRatio: x }` with a positive `x` to `Producer` to emit merge-patch deltas while a group's deltas stay within `x` times the size of a fresh snapshot; `deltaRatio` defaults to `8` when unset. Set it to `0` to disable deltas: every change becomes a fresh single-frame snapshot group. Arrays are replaced wholesale within a delta; a value set to `null` falls back to a snapshot, since merge patch reads `null` as a key deletion.
