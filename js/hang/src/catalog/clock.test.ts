import { expect, test } from "bun:test";
import { ClockSchema, MOQ_EPOCH_UNIX_MILLIS, u53, wallClockTime } from "./index.ts";
import { RootSchema } from "./root.ts";

// Mirrors rs/hang/fixtures/catalog-clock.json: the canonical new shape, a root clock plus an
// archive without a wall field. A reader maps archive timestamps through the root clock after
// timescale conversion.
const FIXTURE = {
	video: {
		renditions: {
			video: { codec: "avc1.64001f", container: { kind: "legacy" } },
		},
	},
	audio: {
		renditions: {
			audio: { codec: "opus", container: { kind: "legacy" }, sampleRate: 48000, numberOfChannels: 2 },
		},
	},
	clock: { wall: 1_751_846_400_000_000, timescale: 1_000_000 },
	archive: { track: "timeline.z", timescale: 1000, durationMax: 2000 },
};

test("timescale defaults to microseconds", () => {
	const parsed = ClockSchema.parse({ wall: 1000 });
	expect(parsed.timescale).toBe(u53(1_000_000));
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

test("wallClockTime refuses bad inputs", () => {
	const clock = ClockSchema.parse({ wall: 0 });
	expect(() => wallClockTime(clock, 0, 0)).toThrow();
	// A zero timescale that slipped past parsing still refuses at conversion.
	const bad = JSON.parse('{"wall":0,"timescale":0}');
	expect(() => wallClockTime(bad, 0, 1000)).toThrow();
	expect(() => wallClockTime(clock, Number.MAX_SAFE_INTEGER, 1)).toThrow();
});

test("the packaged shape maps archive timestamps through the root clock", () => {
	const catalog = RootSchema.parse(FIXTURE);
	if (!catalog.clock) throw new Error("fixture lost its clock");
	if (!catalog.archive) throw new Error("fixture lost its archive");

	// Archive PTS 2000 (ms) lands 2s after the wall epoch.
	expect(wallClockTime(catalog.clock, 2000, catalog.archive.timescale).getTime()).toBe(
		MOQ_EPOCH_UNIX_MILLIS + 1_751_846_402_000,
	);
	expect("wall" in catalog.archive).toBe(false);
});
