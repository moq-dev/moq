import { expect, test } from "bun:test";
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
	const delta: Has<"deltas", SnapshotConsumerInit> = false;
	const initial: Has<"initial", SnapshotConsumerInit> = false;
	const op: Has<"ops", WindowConsumerInit> = false;
	const checkpoint: Has<"checkpointRecords", WindowConsumerInit> = false;
	expect([delta, initial, op, checkpoint]).toEqual([false, false, false, false]);

	const track: Has<"track", SnapshotConsumerInit> = true;
	const schema: Has<"schema", SnapshotConsumerInit> = true;
	expect([track, schema]).toEqual([true, true]);

	const producerDelta: Has<"deltas", SnapshotProducerInit> = true;
	const producerOp: Has<"ops", WindowProducerInit> = true;
	expect([producerDelta, producerOp]).toEqual([true, true]);

	// Compression is the track's, not a codec option.
	const snapshotCompression: Has<"compression", SnapshotProducerInit> = false;
	const streamCompression: Has<"compression", StreamProducerInit> = false;
	const windowCompression: Has<"compression", WindowProducerInit> = false;
	expect([snapshotCompression, streamCompression, windowCompression]).toEqual([false, false, false]);
});

test("object literals reject producer knobs and positional tracks", () => {
	const track = new Track.Producer("test").subscribe();
	const reject = () => {
		// @ts-expect-error deltas is producer-only
		new Snapshot.Consumer({ track, deltas: true });
		// @ts-expect-error initial is producer-only
		new Snapshot.Consumer({ track, initial: {} });
		// @ts-expect-error a positional track is not an options object
		new Snapshot.Consumer(track);
		// @ts-expect-error a producer track is not a subscriber
		new Snapshot.Consumer({ track: new Track.Producer("test") });
		// @ts-expect-error ops is producer-only
		new Window.Consumer({ track, ops: true });
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
		// @ts-expect-error compression is set on the track
		new Snapshot.Producer({ track: new Track.Producer("test"), compression: "deflate" });
		// @ts-expect-error compression is set on the track
		new Stream.Producer({ track: new Track.Producer("test"), compression: "deflate" });
	};
	expect(reject).toBeDefined();
});
