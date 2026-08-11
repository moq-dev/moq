import { describe, expect, it } from "bun:test";
import type * as Catalog from "@moq/hang/catalog";
import type { Time } from "@moq/net";
import { caughtUp, renditionJitter } from "./jitter";

// `jitter` is a branded u53 in the catalog schema, so build the config through a cast.
function config(props: { jitter?: number; framerate?: number }): Catalog.VideoConfig {
	return { codec: "avc1.640028", container: { kind: "legacy" }, ...props } as Catalog.VideoConfig;
}

const ms = (value: number) => value as Time.Milli;

describe("renditionJitter", () => {
	it("prefers the catalog value", () => {
		expect(renditionJitter(config({ jitter: 2000, framerate: 30 }))).toBe(ms(2000));
	});

	it("falls back to a frame interval", () => {
		expect(renditionJitter(config({ framerate: 30 }))).toBe(ms(34));
	});

	it("is undefined when the catalog declares neither", () => {
		expect(renditionJitter(config({}))).toBeUndefined();
	});
});

describe("caughtUp", () => {
	it("accepts a playhead trailing live by less than the rendition's jitter", () => {
		// A 2s segmented rendition sits up to a segment behind live even when fully caught up.
		expect(caughtUp({ playhead: ms(8500), live: ms(10_000), jitter: ms(2000) })).toBe(true);
	});

	it("rejects a playhead trailing live by more than the rendition's jitter", () => {
		expect(caughtUp({ playhead: ms(7400), live: ms(10_000), jitter: ms(2000) })).toBe(false);
	});

	it("does not lend a low-jitter rendition the allowance of a coarse one", () => {
		expect(caughtUp({ playhead: ms(9500), live: ms(10_000), jitter: ms(34) })).toBe(false);
		expect(caughtUp({ playhead: ms(9900), live: ms(10_000), jitter: ms(34) })).toBe(true);
	});

	it("accepts a playhead at or ahead of live", () => {
		expect(caughtUp({ playhead: ms(10_000), live: ms(10_000), jitter: ms(0) })).toBe(true);
		expect(caughtUp({ playhead: ms(10_500), live: ms(10_000), jitter: ms(0) })).toBe(true);
	});
});
