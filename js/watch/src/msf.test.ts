import { expect, test } from "bun:test";
import { u53 } from "@moq/hang/catalog";
import * as Msf from "@moq/msf";
import { Path } from "@moq/net";
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

test.each(["loc", "legacy"])("derives 80 ms Opus decoder warmup from %s MSF audio", (packaging) => {
	for (const samplerate of [8000, 48000]) {
		const catalog: Msf.Catalog = {
			tracks: [{ name: "audio", packaging, role: "audio", codec: "opus", samplerate, delay: 3, jitter: 4 }],
		};
		const audio = toHang(catalog).audio?.renditions.audio;
		expect(audio?.warmup).toBe(u53(80));
		expect(audio?.sampleRate).toBe(u53(samplerate));
		expect(audio?.delay).toBe(u53(3));
		expect(audio?.jitter).toBe(u53(4));
	}
});

test.each(["mp4a.40.2", "flac", "future-audio"])("keeps MSF decoder warmup absent for %s", (codec) => {
	const audio = toHang({ tracks: [{ name: "audio", packaging: "loc", role: "audio", codec }] }).audio?.renditions
		.audio;
	expect(audio?.codec).toBe(codec);
	expect(audio?.warmup).toBeUndefined();
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

test("rounds a fractional MSF jitter up, keeps zero, and rejects a negative one", () => {
	// moq-dev publishers write jitter as fractional milliseconds: one 30 fps frame is 33.333333.
	const catalog: Msf.Catalog = {
		tracks: [
			{ name: "video", packaging: "loc", role: "video", codec: "vp09.00.10.08", jitter: 33.333333 },
			{ name: "audio", packaging: "loc", role: "audio", codec: "opus", jitter: 20.5 },
			{ name: "zero", packaging: "loc", role: "video", codec: "vp09.00.10.08", jitter: 0 },
		],
	};

	const hang = toHang(catalog);
	expect(hang.video?.renditions.video?.jitter).toBe(u53(34));
	expect(hang.audio?.renditions.audio?.jitter).toBe(u53(21));
	// Unlike delay, zero is kept: without a jitter the player falls back to the frame interval.
	expect(hang.video?.renditions.zero?.jitter).toBe(u53(0));

	for (const jitter of [-1, -0.5]) {
		const negative: Msf.Catalog = {
			tracks: [
				{ name: "video", packaging: "loc", role: "video", codec: "vp09.00.10.08", jitter },
				{ name: "audio", packaging: "loc", role: "audio", codec: "opus", jitter },
			],
		};
		expect(() => toHang(negative)).toThrow();
	}
});

test("maps loc packaging to LOC-04's Object Properties framing, not hang's loc container", () => {
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

	expect(toHang(catalog).video?.renditions.video?.container).toEqual({ kind: "ietf-loc" });
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
		expect(text?.container).toEqual({ kind: "ietf-loc" });
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

test("resolves an MSF namespace relative to its catalog broadcast", () => {
	const catalog = Msf.decode(
		new TextEncoder().encode(
			JSON.stringify({
				version: "draft-01",
				tracks: [{ name: "video", namespace: "room/left", packaging: "loc", role: "video", codec: "vp8" }],
			}),
		),
	);
	const base = Path.from("room/catalog");
	const root = toHang(catalog, base);
	expect(root.video?.renditions.video?.broadcast).toBe(Path.normalizeRelative("./left"));
});

// Example 1 from draft-ietf-moq-msf-00 repeats the publisher's full namespace on every track. A
// subscriber under a different root still reads them from the catalog's own broadcast. The example's
// truncated AV1 codec string is shortened to one the AV1 codecs parameter allows.
test("a draft catalog repeating its own namespace plays from any root", () => {
	const namespace = "conference.example.com/conference123/alice";
	const catalog = Msf.decode(
		new TextEncoder().encode(
			JSON.stringify({
				version: 1,
				generatedAt: 1746104606044,
				tracks: [
					{
						name: "1080p-video",
						namespace,
						packaging: "loc",
						isLive: true,
						targetLatency: 2000,
						role: "video",
						renderGroup: 1,
						codec: "av01.0.08M.10",
						width: 1920,
						height: 1080,
						framerate: 30,
						bitrate: 1500000,
					},
					{
						name: "audio",
						namespace,
						packaging: "loc",
						isLive: true,
						targetLatency: 2000,
						role: "audio",
						codec: "opus",
						samplerate: 48000,
						channelConfig: "2",
						bitrate: 32000,
					},
				],
			}),
		),
	);
	for (const base of [namespace, `anon/${namespace}`, "conference123/alice", "alice"]) {
		const root = toHang(catalog, Path.from(base));
		expect(root.video?.renditions["1080p-video"]).toBeDefined();
		expect(root.video?.renditions["1080p-video"]?.broadcast).toBeUndefined();
		expect(root.audio?.renditions.audio).toBeDefined();
		expect(root.audio?.renditions.audio?.broadcast).toBeUndefined();
	}
});

test("MSF namespaces preserve self and root references and refuse unrepresentable names", () => {
	const track: Msf.Track = { name: "video", packaging: "loc", role: "video", codec: "vp8" };
	const base = Path.from("room/catalog");
	const self = toHang({ tracks: [{ ...track, namespace: base }] }, base).video?.renditions.video;
	expect(self).toBeDefined();
	expect(self?.broadcast).toBeUndefined();
	expect(toHang({ tracks: [{ ...track, namespace: "" }] }, base).video?.renditions.video?.broadcast).toBe(
		Path.normalizeRelative(".."),
	);
	for (const namespace of ["room/..", "room//left", "/room/left", "room/left/"]) {
		expect(() => toHang({ tracks: [{ ...track, namespace }] }, base)).toThrow("cannot be referenced");
	}
	expect(() => toHang({ tracks: [{ ...track, namespace: "room/left" }] })).toThrow(
		"requires the catalog broadcast path",
	);
	const last = toHang(
		{
			tracks: [
				{ ...track, namespace: "room/left" },
				{ ...track, namespace: "room/right" },
			],
		},
		base,
	);
	const broadcast = last.video?.renditions.video?.broadcast;
	expect(broadcast && Path.tryResolve(base, broadcast)).toBe(Path.from("room/right"));
});

test("a later MSF track with a name replaces the earlier one, whatever its role", () => {
	const video: Msf.Track = { name: "main", packaging: "loc", role: "video", codec: "vp8" };
	const audio: Msf.Track = { name: "main", packaging: "loc", role: "audio", codec: "opus", samplerate: 48_000 };
	const root = toHang({ tracks: [video, { ...video, codec: "vp09.00.10.08" }, audio] });
	expect(root.video).toBeUndefined();
	expect(root.audio?.renditions.main?.codec).toBe("opus");
	expect(toHang({ tracks: [video, { ...video, codec: "vp09.00.10.08" }] }).video?.renditions.main?.codec).toBe(
		"vp09.00.10.08",
	);
});
