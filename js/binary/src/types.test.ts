import { expect, test } from "bun:test";
import { Track } from "@moq/net";

import { Snapshot, Stream } from "./index.ts";

type SnapshotConsumerInit = ConstructorParameters<typeof Snapshot.Consumer>[0];
type SnapshotProducerInit = ConstructorParameters<typeof Snapshot.Producer>[0];

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
