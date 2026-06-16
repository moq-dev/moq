import { expect, test } from "bun:test";
import * as Json from "@moq/json";
import { Broadcast as MoqBroadcast } from "@moq/net";
import { type Getter, Signal } from "@moq/signals";
import { JsonConsumer } from "./json.ts";

// Resolve once the signal holds a defined value.
function nextDefined<T>(signal: Getter<T | undefined>): Promise<T> {
	return waitFor(signal, (value): value is T => value !== undefined);
}

// Resolve once the signal's value satisfies the predicate.
function waitFor<T, U extends T>(signal: Getter<T>, predicate: (value: T) => value is U): Promise<U> {
	return new Promise((resolve) => {
		const dispose = signal.subscribe((value) => {
			if (!predicate(value)) return;
			dispose();
			resolve(value);
		});
	});
}

test("reads a custom JSON track from the active broadcast", async () => {
	const broadcast = new MoqBroadcast();
	const active = new Signal<MoqBroadcast | undefined>(broadcast);

	const consumer = new JsonConsumer<{ title: string }>(active, "meta.json");

	// The consumer subscribes; serve the request from the producer side.
	const request = await broadcast.requested();
	if (!request) throw new Error("expected a track request");
	expect(request.track.name).toBe("meta.json");
	const producer = new Json.Producer<{ title: string }>(request.track);
	producer.update({ title: "hello" });

	expect(await nextDefined(consumer.value)).toEqual({ title: "hello" });

	consumer.close();
});

test("clears the value when the broadcast goes away", async () => {
	const broadcast = new MoqBroadcast();
	const active = new Signal<MoqBroadcast | undefined>(broadcast);

	const consumer = new JsonConsumer<{ title: string }>(active, "meta.json");

	const request = await broadcast.requested();
	if (!request) throw new Error("expected a track request");
	const producer = new Json.Producer<{ title: string }>(request.track);
	producer.update({ title: "hello" });
	await nextDefined(consumer.value);

	// Dropping the active broadcast tears down the subscription and clears the value.
	await new Promise<void>((resolve) => {
		const dispose = consumer.value.subscribe((value) => {
			if (value !== undefined) return;
			dispose();
			resolve();
		});
		active.set(undefined);
	});
	expect(consumer.value.peek()).toBeUndefined();

	consumer.close();
});

test("resubscribes when the active broadcast switches", async () => {
	const first = new MoqBroadcast();
	const active = new Signal<MoqBroadcast | undefined>(first);

	const consumer = new JsonConsumer<{ title: string }>(active, "meta.json");

	const req1 = await first.requested();
	if (!req1) throw new Error("expected a track request on the first broadcast");
	new Json.Producer<{ title: string }>(req1.track).update({ title: "first" });
	expect(await nextDefined(consumer.value)).toEqual({ title: "first" });

	// Switching to a new broadcast resubscribes and reads from the new track.
	const second = new MoqBroadcast();
	active.set(second);

	const req2 = await second.requested();
	if (!req2) throw new Error("expected a track request on the second broadcast");
	expect(req2.track.name).toBe("meta.json");
	new Json.Producer<{ title: string }>(req2.track).update({ title: "second" });

	const value = await waitFor(consumer.value, (v): v is { title: string } => v?.title === "second");
	expect(value).toEqual({ title: "second" });

	consumer.close();
});
