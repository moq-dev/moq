import { expect, test } from "bun:test";
import * as z from "@zod/mini";
import { u53 } from "./integers.ts";
import type { ReferenceEpoch, RelativeBroadcast } from "./path.ts";
import { checkRenditions, DuplicateRendition, encode, RootSchema, UnsupportedVersion, VERSION } from "./root.ts";
import { track } from "./track.ts";

// The base catalog carries the media sections (`video`/`audio`) and the data track sections
// (`json`/`binary`). Applications add their own root sections (e.g. `scte35`) without modifying
// hang, relying on the loose schema to pass them through.

test("base catalog preserves unknown sections", () => {
	const extended = { video: { renditions: {} }, scte35: { spliceId: 42 } };
	const parsed = RootSchema.parse(extended) as Record<string, unknown>;
	// A base consumer validates the known fields but keeps the unknown section untouched.
	expect(parsed.scte35).toEqual({ spliceId: 42 });
});

test("extended schema validates app sections", () => {
	const Scte35Schema = z.object({ spliceId: z.number() });
	const ExtendedSchema = z.extend(RootSchema, { scte35: z.optional(Scte35Schema) });

	expect(ExtendedSchema.parse({ scte35: { spliceId: 7 } }).scte35).toEqual({ spliceId: 7 });

	// The extended schema enforces the app's section type.
	expect(() => ExtendedSchema.parse({ scte35: { spliceId: "nope" } })).toThrow();
});

test("rendition broadcast reference is parsed and normalized", () => {
	const catalog = {
		video: {
			renditions: {
				video: {
					broadcast: "././source",
					codec: "avc1.64001f",
					container: { kind: "legacy" },
				},
			},
		},
	};
	const parsed = RootSchema.parse(catalog);
	if (!parsed.video || !("renditions" in parsed.video)) throw new Error("missing video section");
	// Normalized like Rust path::Relative: redundant `.` segments are dropped.
	expect(parsed.video.renditions.video?.broadcast).toBe("source" as RelativeBroadcast);

	// Spelled like a path, so an empty segment or a lone backslash refuses the catalog.
	for (const broadcast of ["./source/", "/source", "a//b", "a\\b"]) {
		const video = { broadcast, codec: "avc1.64001f", container: { kind: "legacy" } };
		expect(RootSchema.safeParse({ video: { renditions: { video } } }).success).toBe(false);
	}
});

// Mirrors Rust's `reference_epochs_roundtrip_in_every_section`.
test("every reference-bearing section carries an optional epoch", () => {
	const EPOCH = "01890a5d-ac96-774b-bcce-b302099a8057" as ReferenceEpoch;
	const catalog = (epoch: string) => ({
		video: {
			renditions: {
				v: { broadcast: "./a", epoch, codec: "avc1.64001f", container: { kind: "legacy" } },
			},
		},
		audio: {
			renditions: {
				a: {
					broadcast: "./a",
					epoch,
					codec: "opus",
					sampleRate: 48000,
					numberOfChannels: 2,
					container: { kind: "legacy" },
				},
			},
		},
		text: { renditions: { t: { broadcast: "./a", epoch, format: "vtt" } } },
		json: { tracks: { j: { broadcast: "./a", epoch, mode: "snapshot" } } },
		binary: { tracks: { b: { broadcast: "./a", epoch, mode: "snapshot" } } },
	});

	const parsed = RootSchema.parse(catalog(EPOCH));
	if (!parsed.video || !("renditions" in parsed.video)) throw new Error("missing video section");
	if (!parsed.audio || !("renditions" in parsed.audio)) throw new Error("missing audio section");
	expect(parsed.video.renditions.v?.epoch).toBe(EPOCH);
	expect(parsed.audio.renditions.a?.epoch).toBe(EPOCH);
	expect(parsed.text?.renditions.t?.epoch).toBe(EPOCH);
	expect(parsed.json?.tracks.j?.epoch).toBe(EPOCH);
	expect(parsed.binary?.tracks.b?.epoch).toBe(EPOCH);

	// A malformed or noncanonical epoch refuses the catalog, as in Rust.
	for (const bad of ["not-a-uuid", EPOCH.toUpperCase(), "01890a5d-ac96-474b-bcce-b302099a8057"]) {
		expect(RootSchema.safeParse(catalog(bad)).success).toBe(false);
	}
});

test("rendition parent broadcast reference stays distinct from empty", () => {
	const catalog = {
		video: {
			renditions: {
				video: {
					broadcast: ".",
					codec: "avc1.64001f",
					container: { kind: "legacy" },
				},
			},
		},
	};
	const parsed = RootSchema.parse(catalog);
	if (!parsed.video || !("renditions" in parsed.video)) throw new Error("missing video section");
	expect(parsed.video.renditions.video?.broadcast).toBe("." as RelativeBroadcast);
});

test("rendition without broadcast reference stays undefined", () => {
	const catalog = {
		video: {
			renditions: {
				video: { codec: "avc1.64001f", container: { kind: "legacy" } },
			},
		},
	};
	const parsed = RootSchema.parse(catalog);
	if (!parsed.video || !("renditions" in parsed.video)) throw new Error("missing video section");
	expect(parsed.video.renditions.video?.broadcast).toBeUndefined();
});

test("legacy zero jitter is absent for audio and video", () => {
	const parsed = RootSchema.parse({
		audio: {
			renditions: {
				audio: {
					codec: "opus",
					container: { kind: "legacy" },
					sampleRate: 48000,
					numberOfChannels: 2,
					jitter: 0,
				},
			},
		},
		video: {
			renditions: { video: { codec: "avc1.64001f", container: { kind: "legacy" }, framerate: 30, jitter: 0 } },
		},
	});
	expect(parsed.audio?.renditions.audio?.jitter).toBeUndefined();
	expect(parsed.video?.renditions.video?.jitter).toBeUndefined();
	expect(JSON.stringify(parsed)).not.toContain('"jitter"');
});

test("delay parses beside jitter and zero is absent", () => {
	const parsed = RootSchema.parse({
		audio: {
			renditions: {
				audio: {
					codec: "opus",
					container: { kind: "legacy" },
					sampleRate: 48000,
					numberOfChannels: 2,
					delay: 0,
				},
			},
		},
		video: {
			renditions: { video: { codec: "avc1.64001f", container: { kind: "legacy" }, jitter: 34, delay: 200 } },
		},
		text: { renditions: { captions: { format: "vtt", container: { kind: "legacy" }, delay: 120 } } },
		json: { tracks: { gps: { mode: "stream", jitter: 10, delay: 250 } } },
		binary: { tracks: { frames: { mode: "snapshot", delay: 0 } } },
	});
	expect(parsed.json?.tracks.gps?.delay).toBe(u53(250));
	expect(parsed.binary?.tracks.frames?.delay).toBeUndefined();
	expect(parsed.audio?.renditions.audio?.delay).toBeUndefined();
	expect(parsed.video?.renditions.video?.delay).toBe(u53(200));
	expect(parsed.text?.renditions.captions?.delay).toBe(u53(120));
});

test("clock round-trips at the root", () => {
	const parsed = RootSchema.parse({
		clock: { wall: 1_751_846_400_000_000, timescale: 1_000_000 },
	});
	expect(parsed.clock).toMatchObject({ wall: 1_751_846_400_000_000, timescale: 1_000_000 });
});

test("clock stays off the wire when absent", () => {
	expect(JSON.stringify(RootSchema.parse({}))).not.toContain('"clock"');
});

test("a rendition's hls field round-trips", () => {
	const hls = { track: "video.hls", duration: 2000 };
	const parsed = RootSchema.parse({
		video: { renditions: { video: { codec: "avc1.64001f", container: { kind: "legacy" }, hls } } },
	});
	if (!parsed.video || !("renditions" in parsed.video)) throw new Error("missing video section");
	expect(parsed.video.renditions.video?.hls).toEqual({ track: hls.track, duration: u53(hls.duration) });
	expect(JSON.stringify(parsed)).toContain('"hls":{"track":"video.hls","duration":2000}');
});

test("a malformed hls field leaves the rendition playable", () => {
	const config = { codec: "avc1.64001f", container: { kind: "legacy" } };
	for (const hls of [{ track: "video.hls" }, { duration: 2000 }, { track: 42, duration: 2000 }, "video.hls", null]) {
		const parsed = RootSchema.parse({ video: { renditions: { video: { ...config, hls } } } });
		if (!parsed.video || !("renditions" in parsed.video)) throw new Error("missing video section");
		expect(parsed.video.renditions.video?.hls).toBeUndefined();
		expect(parsed.video.renditions.video?.codec).toBe(config.codec);
	}
});

// The root `archive` section is gone: a catalog from an older publisher still parses, with its
// media intact and the entry left as an unknown extension.
test("an old root archive is ignored", () => {
	const video = { renditions: { video: { codec: "avc1.64001f", container: { kind: "legacy" } } } };
	const media = RootSchema.parse({ video });
	const parsed = RootSchema.parse({
		video,
		archive: { timelines: { video: { track: "video.timeline.z", duration: 2000 } }, timescale: 1000 },
	});
	expect(parsed.video).toEqual(media.video);
});

test("an absent or foreign text section parses to empty", () => {
	// `text` was an ordinary application key before captions reserved it, so a value that isn't
	// a section costs its captions and nothing else: video and audio keep playing.
	expect(RootSchema.parse({ video: { renditions: {} } }).text).toBeUndefined();
	for (const legacy of ["a caption overlay", ["a", "b"], { overlay: { x: 1 } }, { renditions: 42 }]) {
		const parsed = RootSchema.parse({ video: { renditions: {} }, text: legacy });
		expect(parsed.text).toBeUndefined();
		expect(parsed.video).toBeDefined();
	}
});

test("a malformed text rendition rejects the catalog", () => {
	// The counterpart: the fallback covers someone else's key, not our own bugs. A value that IS
	// a text section still has to decode, or a rendition with no format would silently cost a
	// publisher its captions in the browser while Rust refuses the same catalog.
	expect(() => RootSchema.parse({ text: { renditions: { captions: { container: { kind: "legacy" } } } } })).toThrow();
});

const avc = { codec: "avc1.64001f", container: { kind: "legacy" } };
const opus = { codec: "opus", sampleRate: 48000, numberOfChannels: 2, container: { kind: "legacy" } };

test("a rendition without a track name subscribes to its ID", () => {
	const parsed = RootSchema.parse({ video: { renditions: { hd: avc } }, audio: { renditions: { en: opus } } });
	const video = parsed.video?.renditions.hd;
	const audio = parsed.audio?.renditions.en;
	if (!video || !audio) throw new Error("missing rendition");
	expect(track("hd", video)).toBe("hd");
	expect(track("en", audio)).toBe("en");
	expect(JSON.stringify(parsed)).not.toContain('"track"');
});

test("one track name in two broadcasts keeps both renditions", () => {
	const catalog = {
		video: {
			renditions: {
				left: { ...avc, broadcast: "./left", track: "video" },
				right: { ...avc, broadcast: "./right", track: "video" },
			},
		},
	};
	const parsed = checkRenditions(RootSchema.parse(catalog));
	for (const id of ["left", "right"]) {
		const config = parsed.video?.renditions[id];
		if (!config) throw new Error(`missing ${id}`);
		expect(track(id, config)).toBe("video");
		expect(config.broadcast as string).toBe(id);
	}
	expect(RootSchema.parse(JSON.parse(JSON.stringify(parsed)))).toEqual(parsed);
});

test("an ID used by both video and audio is refused", () => {
	const parsed = RootSchema.parse({ video: { renditions: { main: avc } }, audio: { renditions: { main: opus } } });
	expect(() => checkRenditions(parsed)).toThrow(DuplicateRendition);
});

// A newer draft may reshape a section, so the version is refused before any section is read,
// wherever it sits in the object, including through an extended schema.
test("a newer catalog version is refused", () => {
	const reshaped = { video: { renditions: { hd: { codec: 42 } } }, version: 5 };
	expect(() => RootSchema.parse(reshaped)).toThrow(UnsupportedVersion);
	const ExtendedSchema = z.extend(RootSchema, { scte35: z.optional(z.string()) });
	expect(() => ExtendedSchema.parse(reshaped)).toThrow(UnsupportedVersion);
});

test("a supported catalog version is read", () => {
	for (const version of [undefined, 3, VERSION]) expect(RootSchema.parse({ version }).version).toBe(version);
	for (const version of ["4", -1, 4.5]) {
		expect(() => RootSchema.parse({ version })).toThrow(z.core.$ZodError);
	}
});

// Only a field an older reader would misread raises the version.
test("the catalog version is needed only for misread fields", () => {
	const audio = { ...opus, warmup: 80 };
	const root = (hd: object) =>
		RootSchema.parse({ video: { renditions: { hd } }, audio: { renditions: { en: audio } } });

	expect(encode(root(avc)).version).toBeUndefined();
	expect(encode(root({ ...avc, track: "hd" })).version).toBeUndefined();
	expect(encode(root({ ...avc, track: "video" })).version).toBe(4);
	expect(encode(root({ ...avc, warmup: 500 })).version).toBe(4);
	const data = RootSchema.parse({ json: { tracks: { chat: { mode: "stream", track: "chat.json" } } } });
	expect(encode(data).version).toBe(4);
	// A carried version is recomputed, never trusted.
	expect(encode({ ...root(avc), version: 4 }).version).toBeUndefined();
});

test("shared stats and echo discovery round trips without changing old catalogs", async () => {
	const fixture = await Bun.file(
		new URL("../../../../rs/hang/fixtures/catalog-stats-echo.json", import.meta.url),
	).json();
	expect(RootSchema.parse(fixture)).toEqual(fixture);
	expect(RootSchema.parse({})).toEqual({});
});

test("a foreign stats or echo value decodes as absent instead of failing the catalog", () => {
	for (const foreign of [{}, 5, "on", { track: 5 }, { path: ["a"] }, null]) {
		for (const key of ["stats", "echo"]) {
			const parsed = RootSchema.parse({ video: { renditions: {} }, [key]: foreign });
			expect(parsed.stats).toBeUndefined();
			expect(parsed.echo).toBeUndefined();
		}
	}
	const parsed = RootSchema.parse({ stats: { track: "stats.json" }, echo: { path: "viewers" } });
	expect(parsed.stats).toEqual({ track: "stats.json" });
	expect(parsed.echo?.path).toBe("viewers" as RelativeBroadcast);
});
