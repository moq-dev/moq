import { expect, test } from "bun:test";
import * as z from "@zod/mini";
import { ARCHIVE_VERSION } from "./archive.ts";
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
	// Normalized like Rust PathRelative: redundant `.` and empty segments are dropped.
	expect(parsed.video.renditions.video?.broadcast).toBe("source");
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
	expect(parsed.video.renditions.video?.broadcast).toBe(".");
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
