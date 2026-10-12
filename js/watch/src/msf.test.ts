import { expect, test } from "bun:test";
import { u53 } from "@moq/hang/catalog";
import type * as Msf from "@moq/msf";
import { toHang } from "./msf";

test("copies delay onto the hang rendition", () => {
	const catalog: Msf.Catalog = {
		tracks: [
			{
				name: "video",
				packaging: "loc",
				role: "video",
				codec: "vp09.00.10.08",
				delay: 200,
				jitter: 40,
			},
			{
				name: "audio",
				packaging: "loc",
				role: "audio",
				codec: "opus",
				delay: 80,
			},
		],
	};

	expect(toHang(catalog).video?.renditions.video?.delay).toBe(u53(200));
	expect(toHang(catalog).video?.renditions.video?.jitter).toBe(u53(40));
	expect(toHang(catalog).audio?.renditions.audio?.delay).toBe(u53(80));
});

test("rounds a fractional MSF delay up, and drops zero", () => {
	const catalog: Msf.Catalog = {
		tracks: [
			{
				name: "video",
				packaging: "loc",
				role: "video",
				codec: "vp09.00.10.08",
				delay: 200.2,
			},
			{
				name: "audio",
				packaging: "loc",
				role: "audio",
				codec: "opus",
				delay: 0.2,
			},
			{
				name: "early",
				packaging: "loc",
				role: "audio",
				codec: "opus",
				delay: 0,
			},
		],
	};

	const hang = toHang(catalog);
	expect(hang.video?.renditions.video?.delay).toBe(u53(201));
	expect(hang.audio?.renditions.audio?.delay).toBe(u53(1));
	expect(hang.audio?.renditions.early?.delay).toBeUndefined();
});

test("keeps loc packaging as the loc container", () => {
	const catalog: Msf.Catalog = {
		tracks: [
			{
				name: "video",
				packaging: "loc",
				role: "video",
				codec: "vp09.00.10.08",
			},
		],
	};

	expect(toHang(catalog).video?.renditions.video?.container).toEqual({ kind: "loc" });
});

test("keeps legacy packaging as the legacy container", () => {
	const catalog: Msf.Catalog = {
		tracks: [
			{
				name: "video",
				packaging: "legacy",
				role: "video",
				codec: "vp09.00.10.08",
			},
		],
	};

	expect(toHang(catalog).video?.renditions.video?.container).toEqual({ kind: "legacy" });
});

test("drops a cmaf rendition without a usable init segment", () => {
	const track: Msf.Track = {
		name: "video",
		packaging: "cmaf",
		role: "video",
		codec: "vp09.00.10.08",
	};

	expect(toHang({ tracks: [track] }).video?.renditions.video).toBeUndefined();
	expect(toHang({ tracks: [{ ...track, initData: "not base64!" }] }).video?.renditions.video).toBeUndefined();
});

test("drops a rendition whose packaging is unknown", () => {
	const catalog: Msf.Catalog = {
		tracks: [
			{
				name: "video",
				packaging: "future",
				role: "video",
				codec: "vp09.00.10.08",
			},
		],
	};

	expect(toHang(catalog).video?.renditions.video).toBeUndefined();
});

test("converts timed text without guessing its format", () => {
	for (const codec of ["vtt", "ttml", "utf8", "future-text"]) {
		const track = { name: "subs", packaging: "loc", role: "caption", codec, lang: "en", label: "SDH" };
		const text = toHang({ tracks: [track] }).text?.renditions.subs;
		expect(text?.format).toBe(codec);
		expect(text?.role).toBe("caption");
		expect(text?.lang).toBe("en");
		expect(text?.label).toBe("SDH");
		expect(text?.container).toEqual({ kind: "loc" });
	}
});

test("custom roles map only when the codec identifies text", () => {
	for (const role of ["commentary", "audiodescription", "signlanguage"]) {
		const catalog = toHang({ tracks: [{ name: "track", packaging: "legacy", role, codec: "opus" }] });
		expect(catalog.text).toBeUndefined();
	}
	const text = toHang({ tracks: [{ name: "text", packaging: "legacy", role: "commentary", codec: "utf8" }] });
	expect(text.text?.renditions.text?.role).toBe("commentary");
});

test("unsupported optional text packaging keeps playable audio and video", () => {
	const tracks = [
		{ name: "video", packaging: "loc", role: "video", codec: "vp09.00.10.08" },
		{ name: "audio", packaging: "loc", role: "audio", codec: "opus", samplerate: 48000, channelConfig: "2" },
	];
	for (const role of ["caption", "subtitle", "commentary"]) {
		const catalog = toHang({ tracks: [...tracks, { name: "text", packaging: "future", role, codec: "vtt" }] });
		expect(catalog.video?.renditions.video?.codec).toBe("vp09.00.10.08");
		expect(catalog.audio?.renditions.audio?.codec).toBe("opus");
		expect(catalog.text).toBeUndefined();
	}
});

test("a text track with supported packaging still requires its codec", () => {
	expect(() => toHang({ tracks: [{ name: "text", packaging: "loc", role: "caption" }] })).toThrow("missing codec");
});

test("unsupported text packaging is skipped before its codec is required", () => {
	const catalog = toHang({ tracks: [{ name: "text", packaging: "future", role: "caption" }] });
	expect(catalog.text).toBeUndefined();
});

test("a text track named __proto__ is kept as a rendition", () => {
	const catalog = toHang({ tracks: [{ name: "__proto__", packaging: "loc", role: "caption", codec: "vtt" }] });
	expect(Object.keys(catalog.text?.renditions ?? {})).toEqual(["__proto__"]);
});
