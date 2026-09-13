import { expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { Track } from "@moq/net";

import { Snapshot, Stream } from "./index.ts";

type Has<K extends string, T> = K extends keyof T ? true : false;

type SnapshotConsumerInit = ConstructorParameters<typeof Snapshot.Consumer>[0];
type SnapshotProducerInit = ConstructorParameters<typeof Snapshot.Producer>[0];
type StreamConsumerInit = ConstructorParameters<typeof Stream.Consumer>[0];
type StreamProducerInit = ConstructorParameters<typeof Stream.Producer>[0];

test("track-owning wrappers take one options object", () => {
	const arities = [
		1 as ConstructorParameters<typeof Snapshot.Producer>["length"],
		1 as ConstructorParameters<typeof Snapshot.Consumer>["length"],
		1 as ConstructorParameters<typeof Stream.Producer>["length"],
		1 as ConstructorParameters<typeof Stream.Consumer>["length"],
	];
	expect(arities).toEqual([1, 1, 1, 1]);

	const producerTrack: SnapshotProducerInit["track"] = new Track.Producer("test");
	const consumerTrack: SnapshotConsumerInit["track"] = producerTrack.subscribe();
	expect(producerTrack).toBeDefined();
	expect(consumerTrack).toBeDefined();
});

test("consumer options are not a producer config", () => {
	const consumerTrack: Has<"track", SnapshotConsumerInit> = true;
	const producerTrack: Has<"track", SnapshotProducerInit> = true;
	const consumerCompression: Has<"compression", SnapshotConsumerInit> = true;
	const streamTrack: Has<"track", StreamConsumerInit> = true;
	const streamCompression: Has<"compression", StreamProducerInit> = true;
	expect([consumerTrack, producerTrack, consumerCompression, streamTrack, streamCompression]).toEqual([
		true,
		true,
		true,
		true,
		true,
	]);
});

test("object literals reject positional tracks and producer tracks on consumers", () => {
	const track = new Track.Producer("test").subscribe();
	const reject = () => {
		// @ts-expect-error a positional track is not an options object
		new Snapshot.Consumer(track);
		// @ts-expect-error a positional track is not an options object
		new Snapshot.Producer(new Track.Producer("test"));
		// @ts-expect-error a producer track is not a subscriber
		new Snapshot.Consumer({ track: new Track.Producer("test") });
		// @ts-expect-error a positional track is not an options object
		new Stream.Consumer(track);
		// @ts-expect-error a positional track is not an options object
		new Stream.Producer(new Track.Producer("test"));
		// @ts-expect-error a producer track is not a subscriber
		new Stream.Consumer({ track: new Track.Producer("test") });
	};
	expect(reject).toBeDefined();
});

test("emitted declarations keep a single options-object constructor", () => {
	const dist = join(import.meta.dir, "..", "dist");
	const files = ["snapshot/consumer.d.ts", "snapshot/producer.d.ts", "stream/consumer.d.ts", "stream/producer.d.ts"];
	if (!existsSync(join(dist, files[0] as string))) return;

	for (const file of files) {
		const body = readFileSync(join(dist, file), "utf8");
		expect(body).toContain("constructor(config:");
		expect(body).not.toContain("constructor(track:");
	}
});
