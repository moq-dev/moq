import { expect, test } from "bun:test";
import { Bandwidth } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { holdViewer } from "./viewer";

async function flush() {
	for (let i = 0; i < 10; i++) await Promise.resolve();
}

test("holdViewer claims while the ceiling is nonzero and follows it", async () => {
	const viewers = new Bandwidth.Viewers();
	const registry = new Signal<Bandwidth.Viewers | undefined>(viewers);
	const max = new Signal<number | undefined>(undefined);
	const effect = new Effect();
	try {
		holdViewer(effect, registry, max);
		await flush();
		expect(viewers.target.peek()).toBe(0);

		max.set(3_000_000);
		await flush();
		expect(viewers.target.peek()).toBe(3_000_000);

		max.set(1_000_000);
		await flush();
		expect(viewers.target.peek()).toBe(1_000_000);

		max.set(0);
		await flush();
		expect(viewers.target.peek()).toBe(0);

		max.set(2_000_000);
		await flush();
		expect(viewers.target.peek()).toBe(2_000_000);

		registry.set(undefined);
		await flush();
		expect(viewers.target.peek()).toBe(0);

		registry.set(viewers);
		await flush();
		expect(viewers.target.peek()).toBe(2_000_000);
	} finally {
		effect.close();
	}
	await flush();
	expect(viewers.target.peek()).toBe(0);
	viewers.close();
});

test("holdViewer skips a ceiling the registry refuses, then claims once it fits", async () => {
	const viewers = new Bandwidth.Viewers();
	const other = viewers.watch(Bandwidth.Viewers.MAX_TARGET);
	const max = new Signal<number | undefined>(1);
	const effect = new Effect();
	const warn = console.warn;
	const warnings: unknown[] = [];
	console.warn = (...args) => warnings.push(args);
	try {
		holdViewer(effect, new Signal(viewers), max);
		await flush();
		expect(viewers.target.peek()).toBe(Bandwidth.Viewers.MAX_TARGET);
		expect(warnings.length).toBe(1);

		other.close();
		max.set(2);
		await flush();
		expect(viewers.target.peek()).toBe(2);
	} finally {
		console.warn = warn;
		effect.close();
		viewers.close();
	}
});

test("holdViewer releases its claim when the ceiling turns invalid, and claims again once valid", async () => {
	const viewers = new Bandwidth.Viewers();
	const max = new Signal<number | undefined>(1_000_000);
	const effect = new Effect();
	const warn = console.warn;
	const warnings: unknown[] = [];
	console.warn = (...args) => warnings.push(args);
	try {
		holdViewer(effect, new Signal(viewers), max);
		await flush();
		expect(viewers.target.peek()).toBe(1_000_000);

		max.set(-1);
		await flush();
		expect(viewers.target.peek()).toBe(0);
		expect(warnings.length).toBe(1);

		max.set(2_000_000);
		await flush();
		expect(viewers.target.peek()).toBe(2_000_000);
	} finally {
		console.warn = warn;
		effect.close();
		viewers.close();
	}
});
