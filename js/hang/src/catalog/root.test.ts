import { expect, test } from "bun:test";
import * as z from "@zod/mini";
import { ARCHIVE_VERSION } from "./archive.ts";
import { u53 } from "./integers.ts";
import type { RelativeBroadcast } from "./path.ts";
import { RootSchema } from "./root.ts";

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
					broadcast: "././source/",
					codec: "avc1.64001f",
					container: { kind: "legacy" },
				},
			},
		},
	};
	const parsed = RootSchema.parse(catalog);
	if (!parsed.video || !("renditions" in parsed.video)) throw new Error("missing video section");
	// Normalized like Rust path::Relative: redundant `.` and empty segments are dropped.
	expect(parsed.video.renditions.video?.broadcast).toBe("source" as RelativeBroadcast);
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
	});
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

test("archive round-trips at the root", () => {
	const parsed = RootSchema.parse({
		archive: {
			track: "timeline.z",
			durationMax: 2000,
			replay: "./recordings/clip",
			store: "https://objects.example/rec/",
			version: ARCHIVE_VERSION,
		},
	});
	expect(parsed.archive).toMatchObject({
		track: "timeline.z",
		timescale: 1000,
		durationMax: 2000,
		replay: "recordings/clip",
		store: "https://objects.example/rec/",
		version: 1,
	});
	expect(JSON.stringify(parsed)).not.toContain('"timeline":');
});

test("a legacy root timeline is not an archive", () => {
	const parsed = RootSchema.parse({ timeline: { track: "timeline.z" } });
	expect(parsed.archive).toBeUndefined();
});

test("an invalid store URL is refused", () => {
	expect(() => RootSchema.parse({ archive: { track: "timeline.z", store: "not a url" } })).toThrow();
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
