import { expect, test } from "bun:test";
import { ClockSchema, MOQ_EPOCH_UNIX_MILLIS, u53, wallClockTime } from "./index.ts";
import { RootSchema } from "./root.ts";

// Mirrors rs/hang/fixtures/catalog-clock.json: the canonical shape, a root clock plus a
// per-rendition `hls` field. A reader maps HLS record timestamps (milliseconds) through the root
// clock after timescale conversion.
const FIXTURE = {
	video: {
		renditions: {
			video: {
				codec: "avc1.64001f",
				codedWidth: 1280,
				codedHeight: 720,
				container: { kind: "legacy" },
				hls: { track: "video.hls", duration: 2000 },
			},
		},
	},
	audio: {
		renditions: {
			audio: {
				codec: "opus",
				container: { kind: "legacy" },
				sampleRate: 48000,
				numberOfChannels: 2,
				hls: { track: "audio.hls", duration: 2000 },
			},
		},
	},
	clock: { wall: 1_751_846_400_000_000, timescale: 1_000_000 },
};

test("an omitted timescale means microseconds on the wire", () => {
	expect(Number(ClockSchema.parse({ wall: 1000 }).timescale)).toBe(1_000_000);
});

test("a zero timescale is refused", () => {
	expect(() => ClockSchema.parse({ wall: 0, timescale: 0 })).toThrow();
});

test("an explicit null timescale is refused", () => {
	expect(() => ClockSchema.parse({ wall: 0, timescale: null })).toThrow();
});

test("a timescale past u32 is refused", () => {
	expect(() => ClockSchema.parse({ wall: 0, timescale: 4_294_967_296 })).toThrow();
});

test("a wall past the JSON-safe integers is refused", () => {
	expect(() => ClockSchema.parse({ wall: Number.MAX_SAFE_INTEGER + 1 })).toThrow();
});

test("wallClockTime converts across timescales", () => {
	const clock = ClockSchema.parse({ wall: 1_000_000 });
	const epoch = MOQ_EPOCH_UNIX_MILLIS + 1000;

	// PTS zero is the wall epoch itself.
	expect(wallClockTime(clock, 0, 1000).getTime()).toBe(epoch);

	// One media second later, whatever timescale names it.
	for (const [pts, scale] of [
		[1000, 1000],
		[48_000, 48_000],
		[90_000, 90_000],
	] as const) {
		expect(wallClockTime(clock, pts, scale).getTime()).toBe(epoch + 1000);
	}
});

test("wallClockTime keeps a pts finer than the wall's timescale", () => {
	// A 1 ms pts on a whole-second wall is not floored away converting into the wall's units.
	const clock = ClockSchema.parse({ wall: 1, timescale: 1 });
	expect(wallClockTime(clock, 1, 1000).getTime()).toBe(MOQ_EPOCH_UNIX_MILLIS + 1001);

	// A fraction of a wall unit past the JSON-safe range is still past it.
	const max = ClockSchema.parse({ wall: Number.MAX_SAFE_INTEGER, timescale: 1 });
	expect(() => wallClockTime(max, 1, 1000)).toThrow();
});

test("wallClockTime refuses bad inputs", () => {
	const clock = ClockSchema.parse({ wall: 0 });
	expect(() => wallClockTime(clock, 0, 0)).toThrow();
	// A zero timescale that slipped past parsing still refuses at conversion.
	const bad = JSON.parse('{"wall":0,"timescale":0}');
	expect(() => wallClockTime(bad, 0, 1000)).toThrow();
	expect(() => wallClockTime(clock, Number.MAX_SAFE_INTEGER, 1)).toThrow();
});

test("the packaged shape maps HLS record timestamps through the root clock", () => {
	const catalog = RootSchema.parse(FIXTURE);
	if (!catalog.clock) throw new Error("fixture lost its clock");
	const video = catalog.video?.renditions.video;
	expect(video?.hls).toEqual({ track: "video.hls", duration: u53(2000) });

	// Record PTS 2000 (ms) lands 2s after the wall epoch.
	expect(wallClockTime(catalog.clock, 2000, 1000).getTime()).toBe(MOQ_EPOCH_UNIX_MILLIS + 1_751_846_402_000);
});
