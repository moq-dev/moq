import { expect, test } from "bun:test";
import { Track } from "@moq/net";
import { Effect } from "@moq/signals";
import { Consumer } from "./consumer.ts";
import { Source } from "./source.ts";

test("seeds subscribers and fans out edits", async () => {
	const source = new Source<Record<string, unknown>>({ initial: {} });

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
	const source = new Source<Record<string, unknown>>({ initial: {} });
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
	const source = new Source<Record<string, unknown>>();
	expect(() => source.mutate(() => {})).toThrow();
});
