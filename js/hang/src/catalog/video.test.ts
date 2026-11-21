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

test("video config keeps a zero reorder distinct from an absent one", () => {
	const parse = (reorder?: number) =>
		VideoConfigSchema.safeParse({ codec: "vp8", container: { kind: "legacy" }, reorder });
	expect(parse().data?.reorder).toBeUndefined();
	expect(parse(0).data?.reorder).toBe(Catalog.u8(0));
	expect(parse(256).success).toBe(false);
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

test("video config ignores a legacy stalled flag", () => {
	const parsed = VideoConfigSchema.parse({
		codec: "avc1.64001f",
		container: { kind: "legacy" },
		stalled: true,
	});

	expect("stalled" in parsed).toBe(false);
	expect(parsed.enabled).toBeUndefined();
});

// moq-hang-04 defines `warmup` for audio only, so a video rendition's is an unknown field.
test("a video warmup is ignored", () => {
	const base = { codec: "vp8", container: { kind: "legacy" } };
	for (const warmup of [0, 500, -1, 0.5]) {
		expect("warmup" in VideoConfigSchema.parse({ ...base, warmup })).toBe(false);
	}
});

test("color parses the H.273 code points and HDR10 metadata a Rust publisher writes", () => {
	const color = {
		primaries: 9,
		transfer: 16,
		matrix: 9,
		range: "limited",
		mastering: {
			red: { x: 0.708, y: 0.292 },
			green: { x: 0.17, y: 0.797 },
			blue: { x: 0.131, y: 0.046 },
			white: { x: 0.3127, y: 0.329 },
			maxLuminance: 1000,
			minLuminance: 0.005,
		},
		contentLight: { maxCll: 1000, maxFall: 400 },
	};
	const config = VideoConfigSchema.parse({ codec: "hvc1.2.4.L93.B0", container: { kind: "legacy" }, color });
	expect(config.color).toEqual(color as typeof config.color);
	expect(Catalog.ColorSchema.parse(color)).toEqual(config.color as Catalog.Color);
	expect(VideoConfigSchema.parse({ codec: "vp8", container: { kind: "legacy" } }).color).toBeUndefined();
});

test("color refuses out-of-range code points and light levels", () => {
	const parse = (color: unknown) =>
		VideoConfigSchema.safeParse({ codec: "vp8", container: { kind: "legacy" }, color }).success;
	expect(parse({ primaries: 256 })).toBe(false);
	expect(parse({ contentLight: { maxCll: 65536, maxFall: 0 } })).toBe(false);
	expect(parse({ matrix: 0 })).toBe(true);
	expect(parse({ range: "full" })).toBe(true);
	expect(parse({ range: true })).toBe(false);
});

const vr = (fields: object) => VideoConfigSchema.parse({ codec: "vp8", container: { kind: "legacy" }, ...fields });

test("projection parses the known kinds and is optional", () => {
	expect(vr({}).projection).toBeUndefined();
	for (const projection of [
		{ kind: "rectilinear" },
		{ kind: "equirectangular" },
		{ kind: "equirectangular", fov: 180 },
	] as const) {
		expect(vr({ projection }).projection).toEqual(projection);
	}
	expect(Catalog.ProjectionSchema.parse({ kind: "rectilinear" })).toEqual({ kind: "rectilinear" });
});

test("projection keeps an unknown kind verbatim and ignores unknown fields", () => {
	const raw = { face: "front", kind: "cubemap" };
	expect(vr({ projection: raw }).projection).toEqual({ kind: "unknown", raw });
	// A newer publisher may add a parameter to a known kind; this reader must not refuse it.
	expect(vr({ projection: { kind: "rectilinear", fov: 90 } }).projection).toEqual({ kind: "rectilinear" });
});

test("projection refuses a malformed known kind", () => {
	expect(() => vr({ projection: { fov: 180 } })).toThrow();
	expect(() => vr({ projection: { kind: 5 } })).toThrow();
	for (const fov of ["wide", 0, -90, 360.5, Number.NaN, Number.POSITIVE_INFINITY]) {
		expect(() => vr({ projection: { kind: "equirectangular", fov } })).toThrow();
	}
	for (const fov of [0.5, 180, 360]) {
		expect(vr({ projection: { kind: "equirectangular", fov } }).projection).toEqual({
			kind: "equirectangular",
			fov,
		});
	}
});

test("stereo is optional and keeps unknown packings verbatim", () => {
	expect(vr({}).stereo).toBeUndefined();
	for (const stereo of ["mono", "left-right", "top-bottom", "multiview"]) {
		expect(vr({ stereo }).stereo).toBe(stereo);
	}
	expect(() => vr({ stereo: 2 })).toThrow();
});

test("orientation follows the shown shape", () => {
	const section = (extra: Record<string, unknown> = {}) => VideoSchema.parse({ renditions: {}, ...extra });
	const sized = (codedWidth: number, codedHeight: number) => rendition({ codedWidth, codedHeight });
	expect(Catalog.Video.orientation(section(), sized(1920, 1080))).toBe("landscape");
	expect(Catalog.Video.orientation(section(), sized(1080, 1920))).toBe("portrait");
	expect(Catalog.Video.orientation(section(), sized(720, 720))).toBeUndefined();
	expect(Catalog.Video.orientation(section(), rendition())).toBeUndefined();

	// The display aspect ratio, not the coded size, is the picture's shape.
	const anamorphic = VideoConfigSchema.parse({
		...sized(1440, 1080),
		displayAspectWidth: 9,
		displayAspectHeight: 16,
	});
	expect(Catalog.Video.orientation(section(), anamorphic)).toBe("portrait");

	// Only an odd quarter turn swaps the sides, however the rotation is written.
	for (const [rotation, expected] of [
		[90, "portrait"],
		[-90, "portrait"],
		[180, "landscape"],
		[269, "portrait"],
		[-45, "landscape"],
		[315, "landscape"],
		[-135, "portrait"],
		[225, "portrait"],
		// Just under halfway stays under it.
		[44.99999999999999, "landscape"],
	] as const) {
		expect(Catalog.Video.orientation(section({ rotation }), sized(1920, 1080))).toBe(expected);
	}

	// The display size is the shape after rotation, shared by every rendition.
	const display = section({ display: { width: 1080, height: 1920 } });
	expect(Catalog.Video.orientation(display, sized(1920, 1080))).toBe("portrait");
	expect(Catalog.Orientation.of(1, 1)).toBeUndefined();
});
