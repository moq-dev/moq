import { describe, expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Time } from "@moq/net";
import type { Format } from "./capture";
import { resolve } from "./encoder";

const captured: Format = { sampleRate: 48_000, channelCount: 2 };

describe("resolve", () => {
	test("defaults Opus to 20ms", () => {
		const resolved = resolve(captured, "opus");
		expect(resolved.frameDuration).toBe(Time.Micro(20_000));
		expect(resolved.catalog.jitter).toBe(Catalog.u53(20));
	});

	// The catalog jitter is a whole-millisecond hint, so it used to be the only place the frame
	// duration lived and 2.5ms could not be published at all.
	test("keeps a 2.5ms Opus frame exact and rounds the catalog hint up", () => {
		const resolved = resolve(captured, { mime: "opus", frameDuration: Time.Milli(2.5) });
		expect(resolved.frameDuration).toBe(Time.Micro(2_500));
		expect(resolved.catalog.jitter).toBe(Catalog.u53(3));
	});

	test("carries every Opus frame duration", () => {
		for (const millis of [2.5, 5, 10, 20, 40, 60]) {
			const resolved = resolve(captured, { mime: "opus", frameDuration: Time.Milli(millis) });
			expect(resolved.frameDuration).toBe(Time.Micro(millis * 1000));
		}
	});

	// Otherwise AudioEncoder.configure throws instead, by which point the rendition has already
	// been advertised and the failure lands on a subscriber rather than the caller.
	test("rejects a duration Opus cannot encode", () => {
		for (const millis of [2.5005, 15, 0, -20]) {
			expect(() => resolve(captured, { mime: "opus", frameDuration: Time.Milli(millis) })).toThrow();
		}
	});

	// AAC-LC has a fixed 1024-sample frame, so there is no duration to configure.
	test("leaves AAC without a frame duration", () => {
		const resolved = resolve(captured, "aac");
		expect(resolved.frameDuration).toBeUndefined();
		expect(resolved.catalog.jitter).toBe(Catalog.u53(Math.ceil((1024 / 48_000) * 1000)));
	});
});
