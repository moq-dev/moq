<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/data

[![npm version](https://img.shields.io/npm/v/@moq/data)](https://www.npmjs.com/package/@moq/data)
[![TypeScript](https://img.shields.io/badge/TypeScript-ready-blue.svg)](https://www.typescriptlang.org/)

Helpers for sending metadata over [Media over QUIC](https://moq.dev/) tracks.

Each helper maps an application data structure onto a [`@moq/net`](../net) track, handling snapshots and deltas so a late joiner can reconstruct the current state from the newest group alone.

- **`@moq/data/set`** syncs a `Set`-like collection of arbitrary binary items, encoding changes as `+`/`-` deltas.
- **`@moq/data/json`** re-exports [`@moq/json`](../json) for snapshot/delta JSON publishing. It lives in its own package today and will migrate here over time.

## Set

```ts
import { Producer, Consumer, stringCodec } from "@moq/data/set";

// Publish the set of track names in a broadcast.
const producer = new Producer(track, { codec: stringCodec });
producer.insert("video");
producer.insert("audio");
producer.remove("audio");

// Consume: yields the full set after each change.
const consumer = new Consumer(track.subscribe(), { codec: stringCodec });
for await (const names of consumer) {
	console.log(names); // Set<string>
}
```

Each group is self-contained: its first frame is a full snapshot of every item and any following frames are single `+` (insert) or `-` (remove) deltas applied in order. A consumer jumps to the newest group, reads the snapshot, and replays the deltas.

Items are arbitrary binary data via a `Codec<T>` (`encode`/`decode` to `Uint8Array`). `stringCodec` and `bytesCodec` are provided; supply your own for richer types. Items dedupe by their encoded bytes, so two values with the same encoding are the same member.

Deltas are on by default (`deltaRatio: 2`); a delta is appended while the group stays within `deltaRatio` times the size of a fresh snapshot, otherwise a new snapshot group is started.
