import { describe, expect, it } from "bun:test";
import type * as Catalog from "@moq/hang/catalog";
import type { Time } from "@moq/net";
import { caughtUp, renditionJitter, switchJitter } from "./playhead";

// `jitter` is a branded u53 in the catalog schema, so build the config through a cast.
function config(props: { jitter?: number; framerate?: number }): Catalog.VideoConfig {
	return { codec: "avc1.640028", container: { kind: "legacy" }, ...props } as Catalog.VideoConfig;
}

const ms = (value: number) => value as Time.Milli;

describe("renditionJitter", () => {
	it("prefers the catalog value", () => {
		expect(renditionJitter(config({ jitter: 2000, framerate: 30 }))).toBe(ms(2000));
	});

	it("keeps a declared zero rather than falling back", () => {
		expect(renditionJitter(config({ jitter: 0, framerate: 30 }))).toBe(ms(0));
	});

	it("falls back to a frame interval", () => {
		expect(renditionJitter(config({ framerate: 30 }))).toBe(ms(34));
	});

	it("is undefined when the catalog declares neither", () => {
		expect(renditionJitter(config({}))).toBeUndefined();
	});
});

describe("caughtUp", () => {
	it("promotes immediately when nothing is rendering", () => {
		expect(caughtUp({ playhead: ms(1000) })).toBe(true);
	});

	it("holds off while the new rendition trails the picture it replaces", () => {
		expect(caughtUp({ playhead: ms(8000), active: ms(10_000) })).toBe(false);
		expect(caughtUp({ playhead: ms(9800), active: ms(10_000) })).toBe(false);
	});

	it("promotes once the new rendition is within slack of it", () => {
		expect(caughtUp({ playhead: ms(9900), active: ms(10_000) })).toBe(true);
		expect(caughtUp({ playhead: ms(10_500), active: ms(10_000) })).toBe(true);
	});

	it("never lets a switch replay more than the slack", () => {
		// Selecting a coarser rendition grows the sync buffer, which drops the live edge below the
		// outgoing playhead. Promoting at the live edge would replay everything in between, so the
		// bar stays the outgoing playhead however far live falls behind it.
		expect(caughtUp({ playhead: ms(8000), active: ms(10_000) })).toBe(false);
	});
});

describe("switchJitter", () => {
	it("covers both renditions until promotion", () => {
		expect(switchJitter({ active: ms(20), pending: ms(100) })).toBe(ms(100));
		expect(switchJitter({ active: ms(100), pending: ms(20) })).toBe(ms(100));
	});

	it("settles to the remaining rendition", () => {
		expect(switchJitter({ active: ms(20) })).toBe(ms(20));
		expect(switchJitter({ pending: ms(100) })).toBe(ms(100));
		expect(switchJitter({})).toBeUndefined();
	});
});
