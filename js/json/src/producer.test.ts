import { expect, test } from "bun:test";
import { Track } from "@moq/net";
import { Effect } from "@moq/signals";
import { Consumer } from "./consumer.ts";
import { Producer } from "./producer.ts";

test("a track-less producer seeds subscribers and fans out edits", async () => {
	const source = new Producer<Record<string, unknown>>({ initial: {} });

	// Edit before anyone subscribes: the value is retained, not lost.
	source.mutate((v) => {
		v.video = { renditions: {} };
	});

	const effect = new Effect();
	const track = new Track("catalog.json");
	source.serve(track, effect);
	const consumer = new Consumer<Record<string, unknown>>(track);

	// A new subscriber is seeded with the current value.
	expect((await consumer.next())?.video).toEqual({ renditions: {} });

	// An independent owner edits its own key; the subscriber sees it, the other key untouched.
	source.mutate((v) => {
		v.scte35 = { splices: [] };
	});
	const update = await consumer.next();
	expect(update?.video).toEqual({ renditions: {} });
	expect(update?.scte35).toEqual({ splices: [] });

	effect.close();
});

test("a reconnecting subscriber is seeded with the full current value", async () => {
	const source = new Producer<Record<string, unknown>>({ initial: {} });
	source.mutate((v) => {
		v.video = { renditions: {} };
		v.scte35 = { splices: [] };
	});

	// The first subscription drains and ends...
	const first = new Effect();
	source.serve(new Track("catalog.json"), first);
	first.close();

	// ...and a fresh subscription still gets the current value, not nothing.
	const effect = new Effect();
	const track = new Track("catalog.json");
	source.serve(track, effect);
	const seeded = await new Consumer<Record<string, unknown>>(track).next();
	expect(seeded?.video).toEqual({ renditions: {} });
	expect(seeded?.scte35).toEqual({ splices: [] });

	effect.close();
});

test("mutate without a value or initial throws", () => {
	const source = new Producer<Record<string, unknown>>();
	expect(() => source.mutate(() => {})).toThrow();
});

test("serve() can override compression per subscriber", async () => {
	// One fan-out producer serves the same value both plaintext and compressed.
	const source = new Producer<Record<string, unknown>>({ initial: {} });
	source.mutate((v) => {
		v.video = { renditions: { hd: { codec: "avc1.640028" } } };
	});

	const effect = new Effect();

	const plainTrack = new Track("catalog.json");
	source.serve(plainTrack, effect);
	const plain = await new Consumer<Record<string, unknown>>(plainTrack).next();

	const zTrack = new Track("catalog.json.z");
	source.serve(zTrack, effect, { compression: true });
	const compressed = await new Consumer<Record<string, unknown>>(zTrack, { compression: true }).next();

	// Both tracks reconstruct the identical value despite different wire encodings.
	expect(compressed).toEqual(plain);
	expect(compressed?.video).toEqual({ renditions: { hd: { codec: "avc1.640028" } } });

	effect.close();
});

test("serve() throws on a track-bound producer", () => {
	const producer = new Producer<Record<string, unknown>>(new Track("meta.json"));
	const effect = new Effect();
	expect(() => producer.serve(new Track("other"), effect)).toThrow();
	effect.close();
});
