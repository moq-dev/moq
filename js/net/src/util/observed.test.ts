import { expect, test } from "bun:test";
import { Effect, type Getter, Signal } from "@moq/signals";
import { Follow, Observed } from "./observed";

const flush = async () => {
	for (let i = 0; i < 8; i++) await Promise.resolve();
};

test("peeking holds no demand and only the last observer releases it", () => {
	const value = new Observed(1);
	expect(value.peek()).toBe(1);
	expect(value.used.peek()).toBe(false);
	const first = value.subscribe(() => {});
	const second = value.subscribe(() => {});
	first();
	first();
	expect(value.used.peek()).toBe(true);
	second();
	expect(value.used.peek()).toBe(false);
});

test("reactive one-shot observers preserve demand across a value update", async () => {
	const value = new Observed(1);
	const demand: boolean[] = [];
	const stop = value.used.subscribe((used) => demand.push(used));
	const effect = new Effect((effect) => {
		effect.get(value);
	});
	await flush();
	value.set(2);
	await flush();
	expect(demand).toEqual([true]);
	effect.close();
	await flush();
	expect(demand).toEqual([true, false]);
	stop();
});

test("forwarded demand switches sources and releases its old source", async () => {
	const first = new Observed(1);
	const second = new Observed(2);
	const source = new Signal<Getter<number> | undefined>(first);
	const follow = new Follow(source);
	expect(follow.peek()).toBe(1);
	expect(first.used.peek()).toBe(false);
	const seen: (number | undefined)[] = [];
	const stop = follow.subscribe((value) => seen.push(value));
	expect(first.used.peek()).toBe(true);
	source.set(second);
	await flush();
	expect(first.used.peek()).toBe(false);
	expect(second.used.peek()).toBe(true);
	second.set(3);
	await flush();
	expect(seen).toEqual([2, 3]);
	stop();
	stop();
	expect(second.used.peek()).toBe(false);
});

test("one-shot forwarding re-arms once per change without releasing demand", async () => {
	const value = new Observed(0);
	const source = new Signal<Getter<number> | undefined>(value);
	const follow = new Follow(source);
	const seen: (number | undefined)[] = [];
	const next = (value: number | undefined) => {
		seen.push(value);
		if (seen.length < 3) follow.changed(next);
	};
	follow.changed(next);
	for (let i = 1; i <= 3; i++) {
		value.set(i);
		await flush();
		expect(seen).toHaveLength(i);
		expect(value.used.peek()).toBe(i < 3);
	}
	expect(seen).toEqual([1, 2, 3]);
});

test("independent observers may share a callback", async () => {
	const value = new Observed(0);
	let calls = 0;
	const callback = () => {
		calls++;
	};
	const first = value.subscribe(callback);
	const second = value.subscribe(callback);
	first();
	value.set(1);
	await flush();
	expect(calls).toBe(1);
	expect(value.used.peek()).toBe(true);
	second();
	expect(value.used.peek()).toBe(false);
});
