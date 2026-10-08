import { expect, test } from "bun:test";
import { ranked, VideoConfigSchema, VideoSchema } from "./video.ts";

function rendition(size?: { codedWidth: number; codedHeight: number }, bitrate?: number) {
	return VideoConfigSchema.parse({
		codec: "vp8",
		container: { kind: "legacy" },
		...size,
		...(bitrate !== undefined ? { bitrate } : {}),
	});
}

test("ranked orders by picture, then bitrate, then name", () => {
	// Names sort worst first, and insertion order is not name order, so either alone picks wrong.
	const renditions = {
		e: rendition({ codedWidth: 1280, codedHeight: 720 }, 3_000_000),
		a: rendition(undefined, 9_000_000),
		c: rendition({ codedWidth: 1280, codedHeight: 720 }),
		f: rendition({ codedWidth: 1920, codedHeight: 1080 }, 6_000_000),
		b: rendition({ codedWidth: 640, codedHeight: 360 }, 1_000_000),
		d: rendition({ codedWidth: 1280, codedHeight: 720 }, 3_000_000),
	};

	expect(ranked(renditions).map(([name]) => name)).toEqual(["f", "d", "e", "c", "b", "a"]);
});

test("ranked puts a known zero bitrate above an unknown one", () => {
	const renditions = {
		unknown: rendition({ codedWidth: 1280, codedHeight: 720 }),
		zero: rendition({ codedWidth: 1280, codedHeight: 720 }, 0),
	};

	expect(ranked(renditions).map(([name]) => name)).toEqual(["zero", "unknown"]);
});

test("ranked treats a missing dimension as no picture", () => {
	const renditions = {
		partial: VideoConfigSchema.parse({
			codec: "vp8",
			container: { kind: "legacy" },
			codedWidth: 1920,
			bitrate: 1,
		}),
		known: rendition({ codedWidth: 16, codedHeight: 16 }, 1),
	};

	expect(ranked(renditions).map(([name]) => name)).toEqual(["known", "partial"]);
});

test("ranked breaks ties in code point order", () => {
	const same = rendition({ codedWidth: 16, codedHeight: 16 }, 1);
	// U+1F600 sorts after U+E000 by code point (Rust `String`) and before it by UTF-16 code unit.
	const emoji = "\u{1F600}";
	const privateUse = "\uE000";
	expect(emoji < privateUse).toBe(true);
	expect(ranked({ [emoji]: same, [privateUse]: same }).map(([name]) => name)).toEqual([privateUse, emoji]);
});

test("video config accepts canonical display aspect fields", () => {
	const parsed = VideoConfigSchema.parse({
		codec: "avc1.64001f",
		container: { kind: "legacy" },
		displayAspectWidth: 4,
		displayAspectHeight: 3,
	});

	expect(Number(parsed.displayAspectWidth)).toBe(4);
	expect(Number(parsed.displayAspectHeight)).toBe(3);
	expect("displayRatioWidth" in parsed).toBe(false);
	expect("displayRatioHeight" in parsed).toBe(false);
});

test("video config accepts optional enabled state", () => {
	const active = VideoConfigSchema.parse({
		codec: "avc1.64001f",
		container: { kind: "legacy" },
	});
	const disabled = VideoConfigSchema.parse({ ...active, enabled: false });

	expect(active.enabled).toBeUndefined();
	expect(disabled.enabled).toBe(false);
});

test("video config ignores a legacy stalled flag", () => {
	const parsed = VideoConfigSchema.parse({
		codec: "avc1.64001f",
		container: { kind: "legacy" },
		stalled: true,
	});

	expect("stalled" in parsed).toBe(false);
	expect(parsed.enabled).toBeUndefined();
});

test("video config accepts a human-readable label", () => {
	const config = VideoConfigSchema.parse({
		label: "Main camera",
		codec: "vp8",
		container: { kind: "legacy" },
	});

	expect(config.label).toBe("Main camera");
});

test("legacy video arrays derive display size from display aspect fields", () => {
	const parsed = VideoSchema.parse([
		{
			track: { name: "video" },
			config: {
				codec: "avc1.64001f",
				container: { kind: "legacy" },
				displayAspectWidth: 16,
				displayAspectHeight: 9,
			},
		},
	]);

	expect(
		parsed.display && {
			width: Number(parsed.display.width),
			height: Number(parsed.display.height),
		},
	).toEqual({ width: 16, height: 9 });
	expect(Number(parsed.renditions.video?.displayAspectWidth)).toBe(16);
	expect(Number(parsed.renditions.video?.displayAspectHeight)).toBe(9);
});
