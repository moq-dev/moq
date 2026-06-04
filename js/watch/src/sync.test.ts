import { describe, expect, it } from "bun:test";
import type { Time } from "@moq/net";
import { Sync } from "./sync";

// Effects in @moq/signals flush on a microtask, so let pending updates drain before asserting.
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("latency range", () => {
	it("is collapsed by default (no ceiling)", async () => {
		const sync = new Sync();
		await flush();
		expect(sync.buffered.peek()).toBe(false);
		sync.close();
	});

	it("enters buffered mode when the ceiling is above the floor", async () => {
		const sync = new Sync({ latencyMax: 30_000 as Time.Milli });
		await flush();
		expect(sync.buffered.peek()).toBe(true);
		expect(sync.maxBuffer.peek()).toBe(30_000 as Time.Milli);
		sync.close();
	});

	it("stays collapsed when the ceiling is at or below the floor", async () => {
		// Fixed 200ms floor sits above the 100ms ceiling, so there's no room to buffer.
		const sync = new Sync({ latencyMin: 200 as Time.Milli, latencyMax: 100 as Time.Milli });
		await flush();
		expect(sync.buffered.peek()).toBe(false);
		sync.close();
	});

	it("reacts to a ceiling set after construction", async () => {
		const sync = new Sync();
		await flush();
		expect(sync.buffered.peek()).toBe(false);

		sync.latencyMax.set(30_000 as Time.Milli);
		await flush();
		expect(sync.buffered.peek()).toBe(true);
		sync.close();
	});

	it("treats an undefined ceiling as uncapped", async () => {
		const sync = new Sync({ latencyMax: undefined });
		await flush();
		// Undefined ceiling means buffer indefinitely (no cap), not collapse.
		expect(sync.buffered.peek()).toBe(false); // props undefined falls back to the "real-time" default

		sync.latencyMax.set(undefined);
		await flush();
		expect(sync.buffered.peek()).toBe(true);
		expect(sync.maxBuffer.peek()).toBeUndefined();
		sync.close();
	});

	it("stays collapsed for an explicit real-time ceiling", async () => {
		const sync = new Sync({ latencyMax: "real-time" });
		await flush();
		expect(sync.buffered.peek()).toBe(false);
		sync.close();
	});
});
