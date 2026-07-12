import { expect, test } from "bun:test";
import { videoCatalog } from "./catalog.ts";

const base: VideoEncoderConfig = {
	codec: "hev1.1.6.L93.B0",
	width: 1920,
	height: 1080,
	framerate: 30,
	bitrate: 2_000_000,
};

test("uses the encoder's authoritative codec + hex description when codec+dimensions match", () => {
	// Safari can emit hvc1 for a hev1 request; the catalog must advertise what was produced plus the hvcC
	// (as hex) the watcher's decoder needs to init HEVC.
	const catalog = videoCatalog(base, {
		reqCodec: "hev1.1.6.L93.B0",
		width: 1920,
		height: 1080,
		codec: "hvc1.1.6.L93.B0",
		description: "deadbeef",
	});

	expect(catalog.codec).toBe("hvc1.1.6.L93.B0");
	expect(catalog.description).toBe("deadbeef");
});

test("matches by value, not identity, so bitrate churn (a rebuilt config object) keeps the description", () => {
	// Bandwidth adaptation rebuilds the VideoEncoderConfig ~10x/s as a NEW object with the same codec +
	// dimensions; a value match keeps the HEVC description stable (an identity check would flap it off and
	// make watchers yank HD down to SD).
	const churned: VideoEncoderConfig = { ...base, bitrate: 3_500_000 };
	const catalog = videoCatalog(churned, {
		reqCodec: "hev1.1.6.L93.B0",
		width: 1920,
		height: 1080,
		codec: "hvc1.1.6.L93.B0",
		description: "deadbeef",
	});

	expect(catalog.codec).toBe("hvc1.1.6.L93.B0");
	expect(catalog.description).toBe("deadbeef");
});

test("falls back to the requested codec with no description before the first keyframe", () => {
	const catalog = videoCatalog(base, undefined);

	expect(catalog.codec).toBe("hev1.1.6.L93.B0");
	expect(catalog.description).toBeUndefined();
});

test("keeps the requested codec when the produced config carries no description (VP9/H.264)", () => {
	// A description-less encoder may report a more-qualified codec (e.g. VP9). Keeping the requested string
	// leaves the catalog byte-identical so no watcher rebuilds its decoder over a cosmetic change.
	const cfg: VideoEncoderConfig = { ...base, codec: "vp09.00.10.08" };
	const catalog = videoCatalog(cfg, {
		reqCodec: "vp09.00.10.08",
		width: 1920,
		height: 1080,
		codec: "vp09.00.10.08.01.01.01.01.00",
	});

	expect(catalog.codec).toBe("vp09.00.10.08");
	expect(catalog.description).toBeUndefined();
});

test("ignores a capture whose requested codec no longer matches (real codec change)", () => {
	const catalog = videoCatalog(
		{ ...base, codec: "vp09.00.10.08" },
		{ reqCodec: "hev1.1.6.L93.B0", width: 1920, height: 1080, codec: "hvc1.1.6.L93.B0", description: "deadbeef" },
	);

	expect(catalog.codec).toBe("vp09.00.10.08");
	expect(catalog.description).toBeUndefined();
});

test("ignores a capture whose dimensions no longer match (real resolution change)", () => {
	const catalog = videoCatalog(
		{ ...base, width: 1280, height: 720 },
		{ reqCodec: "hev1.1.6.L93.B0", width: 1920, height: 1080, codec: "hvc1.1.6.L93.B0", description: "deadbeef" },
	);

	expect(catalog.codec).toBe("hev1.1.6.L93.B0");
	expect(catalog.description).toBeUndefined();
});
