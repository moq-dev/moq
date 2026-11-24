<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# @moq/stats

[![npm version](https://img.shields.io/npm/v/@moq/stats)](https://www.npmjs.com/package/@moq/stats)

Read the traffic stats a [Media over QUIC](https://moq.dev/) relay publishes, the JavaScript counterpart of the Rust [`moq-stats`](https://docs.rs/moq-stats) consumer.
See [the stats format](https://doc.moq.dev/concept/stats) for what each track and counter means.

```ts
import type { Broadcast, Origin } from "@moq/net";
import * as Stats from "@moq/stats";

// `origin` is a connection's origin (connect with moq-lite-07 or newer, which lists tracks).
async function nodes(origin: Origin.Reader) {
	const nodes = new Stats.Nodes(origin);
	for (let event = await nodes.next(); event; event = await nodes.next()) {
		// Follow `event.path` with `origin.follow(event.path)` while `event.active`.
		console.log(event.node, event.active);
	}
}

// `broadcast` is one node's stats broadcast, such as `origin.follow(path).active`'s value.
async function traffic(broadcast: Broadcast.Consumer) {
	// The whole node's bucket at the relay's default depth, followed across counter runs.
	const bucket = new Stats.Consumer(broadcast).bucket("");
	for (let update = await bucket.next(); update; update = await bucket.next()) {
		if (update.kind === "frame") console.log(update.frame.epoch, update.frame.tiers[""]?.paths);
	}
}
```

A bucket run's `epoch` changes whenever its counters restart from zero, so start a fresh rate series on a new one.
