import { expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { Track } from "@moq/net";

import { Snapshot, Stream, Window } from "./index.ts";

type Has<K extends string, T> = K extends keyof T ? true : false;

type SnapshotConsumerInit = ConstructorParameters<typeof Snapshot.Consumer>[0];
type WindowConsumerInit = ConstructorParameters<typeof Window.Consumer>[0];
type SnapshotProducerInit = ConstructorParameters<typeof Snapshot.Producer>[0];
type StreamProducerInit = ConstructorParameters<typeof Stream.Producer>[0];
type WindowProducerInit = ConstructorParameters<typeof Window.Producer>[0];

test("track-owning wrappers take one options object", () => {
	const arities = [
		1 as ConstructorParameters<typeof Snapshot.Producer>["length"],
		1 as ConstructorParameters<typeof Snapshot.Consumer>["length"],
		1 as ConstructorParameters<typeof Stream.Producer>["length"],
		1 as ConstructorParameters<typeof Stream.Consumer>["length"],
		1 as ConstructorParameters<typeof Window.Producer>["length"],
		1 as ConstructorParameters<typeof Window.Consumer>["length"],
	];
	expect(arities).toEqual([1, 1, 1, 1, 1, 1]);

	const producerTrack: SnapshotProducerInit["track"] = new Track.Producer("test");
	const consumerTrack: SnapshotConsumerInit["track"] = producerTrack.subscribe();
	expect(producerTrack).toBeDefined();
	expect(consumerTrack).toBeDefined();
});

test("consumer options omit producer-only knobs", () => {
	const delta: Has<"deltaRatio", SnapshotConsumerInit> = false;
	const initial: Has<"initial", SnapshotConsumerInit> = false;
	const op: Has<"opRatio", WindowConsumerInit> = false;
	const checkpoint: Has<"checkpointRecords", WindowConsumerInit> = false;
	expect([delta, initial, op, checkpoint]).toEqual([false, false, false, false]);

	const track: Has<"track", SnapshotConsumerInit> = true;
	const compression: Has<"compression", SnapshotConsumerInit> = true;
	const schema: Has<"schema", SnapshotConsumerInit> = true;
	expect([track, compression, schema]).toEqual([true, true, true]);

	const producerDelta: Has<"deltaRatio", SnapshotProducerInit> = true;
	const producerOp: Has<"opRatio", WindowProducerInit> = true;
	const streamCompression: Has<"compression", StreamProducerInit> = true;
	expect([producerDelta, producerOp, streamCompression]).toEqual([true, true, true]);
});

test("object literals reject producer knobs and positional tracks", () => {
	const track = new Track.Producer("test").subscribe();
	const reject = () => {
		// @ts-expect-error deltaRatio is producer-only
		new Snapshot.Consumer({ track, deltaRatio: 8 });
		// @ts-expect-error initial is producer-only
		new Snapshot.Consumer({ track, initial: {} });
		// @ts-expect-error a positional track is not an options object
		new Snapshot.Consumer(track);
		// @ts-expect-error a producer track is not a subscriber
		new Snapshot.Consumer({ track: new Track.Producer("test") });
		// @ts-expect-error opRatio is producer-only
		new Window.Consumer({ track, opRatio: 8 });
		// @ts-expect-error checkpointRecords is producer-only
		new Window.Consumer({ track, checkpointRecords: 4 });
		// @ts-expect-error a positional track is not an options object
		new Stream.Consumer(track);
		// @ts-expect-error a positional track is not an options object
		new Stream.Producer(new Track.Producer("test"));
		// @ts-expect-error Encoder does not take a track
		new Snapshot.Encoder({ track: new Track.Producer("test") });
		// @ts-expect-error Decoder does not take a track
		new Snapshot.Decoder({ track });
		// @ts-expect-error Encoder does not take a track
		new Stream.Encoder({ track: new Track.Producer("test") });
		// @ts-expect-error Decoder does not take a track
		new Window.Decoder({ track });
	};
	expect(reject).toBeDefined();
});

test("emitted declarations keep a single options-object constructor", () => {
	const dist = join(import.meta.dir, "..", "dist");
	const files = [
		"snapshot/consumer.d.ts",
		"snapshot/producer.d.ts",
		"stream/consumer.d.ts",
		"stream/producer.d.ts",
		"window/consumer.d.ts",
		"window/producer.d.ts",
	];
	if (!existsSync(join(dist, files[0] as string))) return;

	for (const file of files) {
		const body = readFileSync(join(dist, file), "utf8");
		expect(body).toContain("constructor(config:");
		expect(body).not.toContain("constructor(track:");
	}

	const snapshotConsumer = readFileSync(join(dist, "snapshot/consumer.d.ts"), "utf8");
	expect(snapshotConsumer).not.toContain("deltaRatio");
	expect(snapshotConsumer).not.toContain("initial");

	const windowConsumer = readFileSync(join(dist, "window/consumer.d.ts"), "utf8");
	expect(windowConsumer).not.toContain("opRatio");
	expect(windowConsumer).not.toContain("checkpointRecords");
});
