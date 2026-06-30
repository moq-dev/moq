import type * as Moq from "@moq/net";
import * as z from "zod/mini";

/** Zod schema for a track's wire packaging. Accepts known values or any future string. */
export const PackagingSchema = z.union([
	z.enum(["loc", "cmaf", "legacy", "mediatimeline", "eventtimeline"]),
	z.string(),
]);

/** How a track's frames are packaged on the wire (e.g. "loc" or "cmaf"). */
export type Packaging = z.infer<typeof PackagingSchema>;

/** Zod schema for a track's role. Accepts known values or any future string. */
export const RoleSchema = z.union([
	z.enum(["video", "audio", "audiodescription", "caption", "subtitle", "signlanguage"]),
	z.string(),
]);

/** The semantic role a track plays in the presentation (e.g. "video", "audio", "caption"). */
export type Role = z.infer<typeof RoleSchema>;

// Shared track fields. This is the version-agnostic shape callers see: init data
// is exposed inline via `initData`, regardless of how it was carried on the wire.
const trackShape = {
	name: z.string(),
	packaging: PackagingSchema,
	// draft-00 marks isLive required but omits it on mediatimeline/eventtimeline
	// tracks, so accept its absence rather than reject the whole catalog.
	isLive: z.optional(z.boolean()),
	role: z.optional(RoleSchema),
	codec: z.optional(z.string()),
	width: z.optional(z.number()),
	height: z.optional(z.number()),
	framerate: z.optional(z.number()),
	samplerate: z.optional(z.number()),
	channelConfig: z.optional(z.string()),
	bitrate: z.optional(z.number()),
	/** Resolved base64 initialization data (draft-01's initRef indirection is resolved away). */
	initData: z.optional(z.string()),
	renderGroup: z.optional(z.number()),
	altGroup: z.optional(z.number()),

	// Non-standard: maximum delay (ms) between consecutive frames on this track.
	// The player's buffer must be at least this large to avoid underruns.
	// Mirrors the `jitter` field in the hang catalog.
	jitter: z.optional(z.number()),
};

/** Zod schema describing a single track entry in an MSF catalog. */
export const TrackSchema = z.object(trackShape);

/** A single track in an MSF catalog, including its codec and media properties. */
export type Track = z.infer<typeof TrackSchema>;

/** Zod schema for the top-level MSF catalog: a version-agnostic snapshot of tracks. */
export const CatalogSchema = z.object({
	tracks: z.array(TrackSchema),
});

/** The MSF catalog: a snapshot of the available tracks. */
export type Catalog = z.infer<typeof CatalogSchema>;

/** The newest MSF draft version string this package emits on the wire. */
export const VERSION = "draft-01";

// --- Wire representation (internal) -----------------------------------------
//
// The wire format hides two things from callers: the catalog `version` (number
// in draft-00, "draft-XX" string in draft-01) and init data, which draft-01
// moved out of the track into a root `initDataList` referenced by `initRef`.

const InitDataSchema = z.object({
	id: z.string(),
	type: z.string(),
	data: z.string(),
});

const WireTrackSchema = z.object({
	...trackShape,
	initRef: z.optional(z.string()),
});

const WireCatalogSchema = z.object({
	// draft-00 used the number 1; draft-01 uses a "draft-XX" string. Accept both.
	version: z.union([z.literal(1), z.string()]),
	tracks: z.optional(z.array(WireTrackSchema)),
	initDataList: z.optional(z.array(InitDataSchema)),
});

/** Serialize a catalog to its JSON wire representation (draft-01). */
export function encode(catalog: Catalog): Uint8Array {
	// Hoist inline init payloads into a shared, deduplicated initDataList and
	// reference each from its track via initRef (the draft-01 wire shape).
	const initDataList: z.infer<typeof InitDataSchema>[] = [];
	const ids = new Map<string, string>();

	const tracks = catalog.tracks.map((track) => {
		const { initData, ...rest } = track;
		if (initData === undefined) return rest;

		let id = ids.get(initData);
		if (id === undefined) {
			id = `init${initDataList.length}`;
			initDataList.push({ id, type: "inline", data: initData });
			ids.set(initData, id);
		}
		return { ...rest, initRef: id };
	});

	const wire: Record<string, unknown> = { version: VERSION, tracks };
	if (initDataList.length > 0) wire.initDataList = initDataList;

	return new TextEncoder().encode(JSON.stringify(wire));
}

/** Parse and validate a catalog from its JSON wire representation. Throws if invalid. */
export function decode(raw: Uint8Array): Catalog {
	const str = new TextDecoder().decode(raw);
	try {
		const wire = WireCatalogSchema.parse(JSON.parse(str));

		// id -> inline payload, built once so resolution is linear in the number
		// of tracks rather than tracks x entries.
		const inline = new Map((wire.initDataList ?? []).filter((e) => e.type === "inline").map((e) => [e.id, e.data]));

		const tracks = (wire.tracks ?? []).map(({ initRef, ...track }) => {
			// Resolve draft-01 initRef into inline initData so callers never see
			// the indirection. Inline initData (draft-00) is left untouched.
			if (track.initData === undefined && initRef !== undefined) {
				// Only inline entries are resolved here. A non-inline (e.g. `url`) initRef
				// legitimately stays undefined; the consumer fetches that init out-of-band.
				track.initData = inline.get(initRef);
			}
			return track;
		});

		return { tracks };
	} catch (error) {
		console.warn("invalid MSF catalog", str);
		throw error;
	}
}

/** Read and decode the next catalog frame from a track, or undefined if the track ended. */
export async function fetch(track: Moq.TrackSubscriber): Promise<Catalog | undefined> {
	const frame = await track.readFrame();
	if (!frame) return undefined;
	return decode(frame);
}
