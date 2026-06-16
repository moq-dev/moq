import { expect, test } from "bun:test";
import * as Json from "@moq/json";
import { Track } from "@moq/net";
import { Effect } from "@moq/signals";
import { JsonProducer } from "./json.ts";

test("seeds late subscribers with the current value and fans out updates", async () => {
	const producer = new JsonProducer<{ title?: string; count?: number }>();

	// Set a value before anyone subscribes: it is retained, not lost.
	producer.update({ title: "hello" });
	expect(producer.value).toEqual({ title: "hello" });

	const effect = new Effect();
	const track = new Track("meta.json");
	producer.serve(track, effect);
	const consumer = new Json.Consumer<{ title?: string; count?: number }>(track);

	// A new subscriber is seeded with the current value.
	expect(await consumer.next()).toEqual({ title: "hello" });

	// Subsequent updates fan out to the subscriber.
	producer.update({ title: "world" });
	expect(await consumer.next()).toEqual({ title: "world" });

	// Closing the effect finishes the subscription: it's dropped from the fan-out and the track
	// ends, so further updates never reach it.
	effect.close();
	producer.update({ title: "after close" });
	expect(await consumer.next()).toBeUndefined();
});

test("mutate composes from the last value", async () => {
	const producer = new JsonProducer<Record<string, unknown>>({ initial: {} });

	// mutate works before any update because of the configured initial value.
	producer.mutate((v) => {
		v.a = 1;
	});
	producer.mutate((v) => {
		v.b = 2;
	});

	expect(producer.value).toEqual({ a: 1, b: 2 });
});

test("mutate without a value or initial throws", () => {
	const producer = new JsonProducer<Record<string, unknown>>();
	expect(() => producer.mutate(() => {})).toThrow();
});
