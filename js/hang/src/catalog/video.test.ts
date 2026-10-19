import { expect, test } from "bun:test";
import * as Catalog from "./index.ts";
import { ranked, VideoConfigSchema, VideoSchema } from "./video.ts";

test("ranked puts enabled video before a larger disabled picture", () => {
	const renditions = {
		disabled: { ...rendition({ codedWidth: 1920, codedHeight: 1080 }, 6_000_000), enabled: false },
		implicit: rendition({ codedWidth: 640, codedHeight: 360 }, 1_000_000),
		explicit: { ...rendition({ codedWidth: 1280, codedHeight: 720 }, 3_000_000), enabled: true },
	};
	expect(ranked(renditions).map(([name]) => name)).toEqual(["explicit", "implicit", "disabled"]);
	expect(Catalog.Video.ranked(renditions)).toEqual(ranked(renditions));
});

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

test("ranked puts a higher preference before a larger picture", () => {
	const renditions = {
		fallback: { ...rendition({ codedWidth: 1920, codedHeight: 1080 }, 6_000_000), preference: -1 },
		source: rendition({ codedWidth: 640, codedHeight: 360 }, 1_000_000),
		disabled: { ...rendition({ codedWidth: 16, codedHeight: 16 }, 1), preference: 1, enabled: false },
	};

	expect(ranked(renditions).map(([name]) => name)).toEqual(["source", "fallback", "disabled"]);
});

test("video config accepts a signed 32-bit preference", () => {
	const parse = (preference: number) =>
		VideoConfigSchema.safeParse({ codec: "vp8", container: { kind: "legacy" }, preference });
	expect(parse(-1).data?.preference).toBe(-1);
	expect(parse(1.5).success).toBe(false);
	expect(parse(2 ** 31).success).toBe(false);
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

test("video warmup round trips as optional integer milliseconds", () => {
	const base = { codec: "vp8", container: { kind: "legacy" } };
	expect(VideoConfigSchema.parse(base).warmup).toBeUndefined();
	for (const warmup of [0, 80, 1_000]) {
		const config = VideoConfigSchema.parse({ ...base, warmup });
		expect(Number(config.warmup)).toBe(warmup);
		expect(VideoConfigSchema.parse(JSON.parse(JSON.stringify(config)))).toEqual(config);
	}
	for (const warmup of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1]) {
		expect(() => VideoConfigSchema.parse({ ...base, warmup })).toThrow();
	}
});

const section = (fields: object) => ({ renditions: {}, ...fields });

test("projection parses the known kinds", () => {
	for (const [json, projection] of [
		['{"kind":"rectilinear"}', { kind: "rectilinear" }],
		['{"kind":"equirectangular"}', { kind: "equirectangular" }],
		['{"kind":"equirectangular","fov":180.0}', { kind: "equirectangular", fov: 180 }],
	] as const) {
		expect(VideoSchema.parse(section({ projection: JSON.parse(json) })).projection).toEqual(projection);
	}
});

test("projection is optional", () => {
	expect(VideoSchema.parse(section({})).projection).toBeUndefined();
});

test("unknown projection is preserved instead of throwing", () => {
	const raw = JSON.parse('{"face":"front","kind":"cubemap"}');
	expect(VideoSchema.parse(section({ projection: raw })).projection).toEqual({ kind: "unknown", raw });
});

test("malformed known projection throws", () => {
	for (const json of ['{"kind":"equirectangular","fov":"wide"}', '{"kind":"rectilinear","fov":90}']) {
		expect(() => VideoSchema.parse(section({ projection: JSON.parse(json) }))).toThrow();
	}
});

test("projection fov must be in (0, 360]", () => {
	for (const fov of [0, -90, 360.5, Number.NaN, Number.POSITIVE_INFINITY]) {
		expect(() => VideoSchema.parse(section({ projection: { kind: "equirectangular", fov } }))).toThrow();
	}
	for (const fov of [0.5, 180, 360]) {
		expect(VideoSchema.parse(section({ projection: { kind: "equirectangular", fov } })).projection).toEqual({
			kind: "equirectangular",
			fov,
		});
	}
});

test("stereo parses known layouts and keeps unknown ones verbatim", () => {
	for (const stereo of ["mono", "side-by-side", "top-bottom", "interleaved"]) {
		expect(VideoSchema.parse(section({ stereo })).stereo).toBe(stereo);
	}
	expect(VideoSchema.parse(section({})).stereo).toBeUndefined();
	expect(() => VideoSchema.parse(section({ stereo: 2 }))).toThrow();
});

test("legacy array catalogs have no projection or stereo", () => {
	const parsed = VideoSchema.parse([{ track: { name: "v" }, config: rendition() }]);
	expect(parsed.projection).toBeUndefined();
	expect(parsed.stereo).toBeUndefined();
});
