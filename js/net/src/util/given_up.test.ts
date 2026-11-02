import { expect, test } from "bun:test";
import { yieldNow } from "@moq/clock";
import { Signal } from "@moq/signals";
import { StreamCode, StreamError } from "../error.ts";
import { Producer } from "../group.ts";
import { hooks } from "../internal.ts";
import { GivenUp } from "./given_up.ts";

// A group whose budget verdict the test sets, and how many watches its drift input holds.
function served() {
	const producer = new Producer(0);
	const group = producer.consume();
	const expired = new Signal(false);
	let watching = 0;
	const subscribe = expired.subscribe.bind(expired);
	expired.subscribe = (fn) => {
		watching++;
		const dispose = subscribe(fn);
		return () => {
			watching--;
			dispose();
		};
	};
	hooks.expireGroup(group, { expired: () => expired.peek(), changed: [expired] });
	return { producer, group, expired, watching: () => watching };
}

test("a write that finishes first settles with no reason", async () => {
	const { producer, group, watching } = served();
	const givenUp = new GivenUp(group);
	expect(await givenUp.write(async () => {})).toBeUndefined();
	expect(await givenUp.write(async () => {})).toBeUndefined();
	// One watch for the group, not one per write.
	expect(watching()).toBe(1);
	givenUp.close();
	expect(watching()).toBe(0);
	producer.finish();
});

test("the budget giving up ends a parked write, and every later one before it starts", async () => {
	const { producer, group, expired } = served();
	const givenUp = new GivenUp(group);
	const parked = givenUp.write(() => new Promise(() => {}));
	await yieldNow();
	expired.set(true);
	const stop = await parked;
	expect(stop).toBeInstanceOf(StreamError);
	expect((stop as StreamError).code).toBe(StreamCode.Old);

	let started = false;
	expect(
		await givenUp.write(async () => {
			started = true;
		}),
	).toBe(stop);
	expect(started).toBe(false);
	givenUp.close();
	producer.finish();
});

test("an aborted group ends a parked write with its error", async () => {
	const { producer, group } = served();
	const givenUp = new GivenUp(group);
	const parked = givenUp.write(() => new Promise(() => {}));
	const failed = new Error("upstream failed");
	producer.abort(failed);
	expect(await parked).toBe(failed);
	givenUp.close();
});

test("a clean close is no reason to stop", async () => {
	const { producer, group } = served();
	const givenUp = new GivenUp(group);
	let finish!: () => void;
	const parked = givenUp.write(() => new Promise<void>((resolve) => (finish = resolve)));
	producer.finish();
	await yieldNow();
	finish();
	expect(await parked).toBeUndefined();
	givenUp.close();
});

test("cancel ends a parked write with no reason", async () => {
	const { producer, group } = served();
	const givenUp = new GivenUp(group);
	const cancel = Promise.withResolvers<false>();
	const parked = givenUp.write(() => new Promise(() => {}), cancel.promise);
	cancel.resolve(false);
	expect(await parked).toBeUndefined();
	expect(givenUp.peek()).toBeUndefined();
	givenUp.close();
	producer.finish();
});

test("a closed watch hears no later verdict", async () => {
	const { producer, group, expired, watching } = served();
	const givenUp = new GivenUp(group);
	givenUp.close();
	expect(watching()).toBe(0);
	expired.set(true);
	await yieldNow();
	expect(givenUp.peek()).toBeUndefined();
	producer.finish();
});
