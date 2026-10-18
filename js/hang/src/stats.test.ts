import { expect, test } from "bun:test";
import { Path } from "@moq/net";
import * as z from "@zod/mini";
import * as Echo from "./echo";
import * as Stats from "./stats";

for (const [name, schema] of [
	["../../../rs/hang/fixtures/stats-snapshot.json", Stats.SnapshotSchema],
	["../../../rs/hang/fixtures/echo-snapshot.json", Echo.SnapshotSchema],
	["../../../rs/hang/fixtures/stats-zero.json", Stats.SnapshotSchema],
	["../../../rs/hang/fixtures/echo-zero.json", Echo.SnapshotSchema],
] as const) {
	test(`shared Rust fixture ${name} round trips through JS`, async () => {
		const input = await Bun.file(new URL(name, import.meta.url)).json();
		expect(JSON.parse(JSON.stringify(schema.parse(input)))).toEqual(input);
	});
}

test("missing rendition counters mean zero while parsing preserves omission", () => {
	const input = { renditions: { reset: {} } };
	const stats = Stats.SnapshotSchema.parse(input);
	const echo = Echo.SnapshotSchema.parse(input);
	expect(stats).toEqual(input);
	expect(echo).toEqual(input);
	expect(stats.renditions?.reset.frames ?? 0).toBe(0);
	expect(echo.renditions?.reset.stalled ?? 0).toBe(0);
	expect(stats.renditions?.reset.bitrate).toBeUndefined();
	expect(echo.renditions?.reset.latency).toBeUndefined();
	expect(stats.transport).toBeUndefined();
	expect(Object.hasOwn(stats, "transport")).toBe(false);
	expect(Object.hasOwn(stats.renditions?.reset ?? {}, "frames")).toBe(false);
	expect(Object.hasOwn(stats.renditions?.reset ?? {}, "bitrate")).toBe(false);
	expect(Object.hasOwn(echo.renditions?.reset ?? {}, "arrival")).toBe(false);
	expect(Object.hasOwn(echo.renditions?.reset ?? {}, "latency")).toBe(false);
	expect(Stats.SnapshotSchema.parse({})).toEqual({});
	expect(Echo.SnapshotSchema.parse({})).toEqual({});
});

test("optional null measurements mean unavailable rather than measured zero", () => {
	expect(
		JSON.parse(
			JSON.stringify(
				Stats.SnapshotSchema.parse({
					transport: { rtt: null, rate: null, lostBytes: null, lostPackets: null, age: null },
					renditions: { hd: { bitrate: null } },
				}),
			),
		),
	).toEqual({ transport: {}, renditions: { hd: {} } });
	expect(JSON.parse(JSON.stringify(Echo.RenditionSchema.parse({ latency: null, arrival: null })))).toEqual({});
});

test("unknown track fields are ignored and flattened snapshot extensions survive", () => {
	expect(Stats.RenditionSchema.parse({ future: 42 })).toEqual({});
	expect(Echo.RenditionSchema.parse({ future: 42 })).toEqual({});
	const input = { mpegts: { packets: 42 } };
	expect(Stats.SnapshotSchema.parse(input)).toEqual(input);
	expect(Echo.SnapshotSchema.parse(input)).toEqual(input);
	const extended = z.extend(Echo.SnapshotSchema, { mpegts: z.object({ packets: z.number() }) });
	expect(extended.parse(input)).toEqual(input);
});

test("shared invalid counters refuse instead of rounding", async () => {
	const cases = (await Bun.file(
		new URL("../../../rs/hang/fixtures/snapshot-invalid.json", import.meta.url),
	).json()) as unknown[];
	for (const value of cases) expect(() => Stats.RenditionSchema.parse(value)).toThrow();
	for (const value of [
		{ stalled: -1 },
		{ latency: 0.5 },
		{ arrival: { timestamp: Number.MAX_SAFE_INTEGER + 1, wall: 0 } },
		{ arrival: { timestamp: 0, wall: -1 } },
		{ arrival: {} },
		{ arrival: { timestamp: 0 } },
		{ arrival: { wall: 0 } },
		{ frames: null },
		{ frames: Number.NaN },
	])
		expect(() => Echo.RenditionSchema.parse(value)).toThrow();
	for (const value of [{ rtt: -1 }, { rate: 0.5 }, { lostBytes: Number.MAX_SAFE_INTEGER + 1 }])
		expect(() => Stats.TransportSchema.parse(value)).toThrow();
});

test("feedback discovery agrees with the Rust fixed track and final path suffix", () => {
	expect(Echo.TRACK).toBe("feedback.json");
	expect(Echo.SUFFIX).toBe(".echo");
	expect(Echo.isEcho(Path.from("room/alice.echo"))).toBe(true);
	expect(Echo.isEcho(Path.from("room.echo/alice"))).toBe(false);
	expect(Echo.isEcho(Path.from("room/alice.echoes"))).toBe(false);
});
