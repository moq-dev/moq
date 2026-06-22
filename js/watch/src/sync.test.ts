import { describe, expect, it } from "bun:test";
import type { Time } from "@moq/net";
import { Sync } from "./sync";

// Effects in @moq/signals flush on a microtask, so let pending updates drain before asserting.
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("latency range", () => {
	it("is collapsed by default", async () => {
		const sync = new Sync();
		await flush();
		expect(sync.buffered.peek()).toBe(false);
		sync.close();
	});

	it("stays collapsed for a scalar latency", async () => {
		const sync = new Sync({ latency: 100 as Time.Milli });
		await flush();
		expect(sync.buffered.peek()).toBe(false);
		expect(sync.maxBuffer.peek()).toBe(100 as Time.Milli);
		sync.close();
	});

	it("enters buffered mode when the ceiling is above the floor", async () => {
		const sync = new Sync({ latency: { max: 30_000 as Time.Milli } });
		await flush();
		expect(sync.buffered.peek()).toBe(true);
		expect(sync.maxBuffer.peek()).toBe(30_000 as Time.Milli);
		sync.close();
	});

	it("stays collapsed when the ceiling is at or below the floor", async () => {
		// Fixed 200ms floor sits above the 100ms ceiling, so there's no room to buffer.
		const sync = new Sync({ latency: { min: 200 as Time.Milli, max: 100 as Time.Milli } });
		await flush();
		expect(sync.buffered.peek()).toBe(false);
		sync.close();
	});

	it("reacts to a ceiling set after construction", async () => {
		const sync = new Sync();
		await flush();
		expect(sync.buffered.peek()).toBe(false);

		sync.latency.set({ max: 30_000 as Time.Milli });
		await flush();
		expect(sync.buffered.peek()).toBe(true);
		sync.close();
	});

	it("stays collapsed for an explicit real-time ceiling", async () => {
		const sync = new Sync({ latency: { max: "real-time" } });
		await flush();
		expect(sync.buffered.peek()).toBe(false);
		sync.close();
	});
});
