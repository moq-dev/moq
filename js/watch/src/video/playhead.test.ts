import { describe, expect, it } from "bun:test";
import type * as Catalog from "@moq/hang/catalog";
import type { Time } from "@moq/net";
import { caughtUp, renditionJitter } from "./playhead";

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
		expect(caughtUp({ playhead: ms(1000), live: ms(10_000) })).toBe(true);
	});

	it("falls back to the outgoing playhead before the clock has an anchor", () => {
		expect(caughtUp({ playhead: ms(9800), active: ms(10_000) })).toBe(false);
		expect(caughtUp({ playhead: ms(9950), active: ms(10_000) })).toBe(true);
	});

	it("holds off while the new rendition trails both playheads", () => {
		expect(caughtUp({ playhead: ms(8000), active: ms(10_000), live: ms(10_000) })).toBe(false);
	});

	it("promotes once the new rendition reaches the outgoing playhead", () => {
		expect(caughtUp({ playhead: ms(9950), active: ms(10_000), live: ms(10_000) })).toBe(true);
	});

	it("promotes when playback runs behind live", () => {
		// Delivery is late, so both playheads sit behind live and neither can reach it. The sync
		// reference only ever moves down, so waiting for live would stall the switch outright.
		expect(caughtUp({ playhead: ms(8000), active: ms(8000), live: ms(10_000) })).toBe(true);
	});

	it("promotes when the outgoing playhead sits ahead of live", () => {
		// Switching to a coarser rendition grows the sync buffer, so live drops back. Playheads
		// never rewind, so the outgoing one is stranded ahead of it: waiting for the new
		// rendition to reach it would freeze the picture until wall-clock time caught up.
		expect(caughtUp({ playhead: ms(8000), active: ms(10_000), live: ms(8000) })).toBe(true);
	});

	it("still holds off against a live edge below a stranded outgoing playhead", () => {
		expect(caughtUp({ playhead: ms(6000), active: ms(10_000), live: ms(8000) })).toBe(false);
	});
});
