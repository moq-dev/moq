import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import { Path } from "@moq/net";
import * as z from "zod/mini";
import { AudioSchema } from "./audio";
import { BinarySchema } from "./binary";
import { EchoSchema } from "./echo";
import { TRACK } from "./format";
import { IncludeSchema } from "./include";
import { JsonSchema } from "./json";
import { MediaSchema } from "./media";
import type { RelativeBroadcast } from "./path";
import { PRIORITY } from "./priority";
import { named, object, renditions, SKIPPED, section } from "./section";
import { StatsSchema } from "./stats";
import { TextSchema } from "./text";
import { track } from "./track";
import { UnsupportedVersion, VERSION, VersionSchema } from "./version";
import { VideoSchema } from "./video";

export { UnsupportedVersion, VERSION } from "./version";

// Thrown rather than reported as an issue, so it surfaces as itself and before any section, which
// the newer draft may have reshaped.
const RootVersionSchema = VersionSchema.check(
	z.refine((version) => {
		if (Number.isInteger(version) && version > VERSION) throw new UnsupportedVersion(version);
		return true;
	}),
);

/**
 * The root catalog: the base sections every hang broadcast carries.
 *
 * The media sections are `video`, `audio`, and `text`, alongside `media`, which holds what applies
 * to every audio and video track: the broadcast's `clock` (the one wall-clock mapping) and its
 * `join` interval. `json` and `binary` list application data tracks that aren't media. A section is omitted
 * when it holds no tracks. `stats` names the publisher's media snapshot track, `echo`
 * solicits viewer feedback under a relative broadcast prefix, and `include` lists whole
 * broadcasts without opening them; each shares this catalog's PTS timeline and root settings.
 *
 * This is a *loose* object: unknown root sections pass through validation untouched, so an
 * application can add its own sections (e.g. `scte35`) without modifying hang. A base consumer
 * ignores the extra sections; an extended consumer validates them with its own schema, typically
 * built via `z.extend(RootSchema, { ... })`.
 *
 * The root `version` is the oldest moq-hang draft that reads the catalog correctly, absent while
 * that is 3, and parsing throws {@link UnsupportedVersion} when it is above {@link VERSION}. Each
 * entry (rendition, data track, include) carries its own `version` the same way, and parsing skips
 * an entry whose `version` is above {@link VERSION}; {@link skipped} reports it. A publisher writes
 * the entry versions with {@link encode}.
 */
export const RootSchema = z.looseObject({
	// First, so it is checked before any section.
	version: z.optional(RootVersionSchema),
	video: z.optional(VideoSchema),
	audio: z.optional(AudioSchema),
	// What applies to every audio and video track, omitted when it holds nothing. `media` is a
	// generic key an application may already carry, so a value that isn't an object decodes as
	// absent; an object with a malformed clock fails the catalog, the same as Rust. See `MediaSchema`
	// for its lenient `join`.
	media: object(MediaSchema),
	// Decoded leniently like `text` below: a value without a string `track` or `path` is someone
	// else's and decodes as absent. See `named`.
	stats: named(StatsSchema, "track"),
	echo: named(EchoSchema, "path"),
	// `text`, `json`, and `binary` are generic enough keys that an application could have been
	// carrying its own before these sections were reserved, so a value that isn't a section decodes
	// as absent rather than failing the whole catalog. A value that IS a section but carries a
	// malformed entry (a rendition with no `format`, a track with no `mode`) still fails, the same
	// as Rust, rather than silently dropping every caption or data track. See `section`.
	text: section(TextSchema, "renditions"),
	json: section(JsonSchema, "tracks"),
	binary: section(BinarySchema, "tracks"),
	// A new key, so a malformed entry fails the catalog, the same as Rust.
	include: z.optional(renditions(IncludeSchema, "include")),
});

/** The root catalog object: the media sections, `media`, the data track sections, plus any app extensions. */
export type Root = z.infer<typeof RootSchema>;

/** The catalog entries a reader skipped because a newer moq-hang draft defines them. */
export interface Skipped {
	/** The draft a reader needs to read every skipped entry. */
	version: number;
	/** Skipped video renditions, mapping each ID to its `version`. */
	video: Record<string, number>;
	/** Skipped audio renditions. */
	audio: Record<string, number>;
	/** Skipped text renditions. */
	text: Record<string, number>;
	/** Skipped JSON tracks. */
	json: Record<string, number>;
	/** Skipped binary tracks. */
	binary: Record<string, number>;
	/** Skipped includes. */
	include: Record<string, number>;
}

/**
 * The entries parsing `root` skipped because a newer moq-hang draft defines them, or undefined if
 * it read them all. The rest of the catalog reads as if they were absent, so a player plays what
 * it can and may tell its user to upgrade for the rest.
 *
 * Reads what {@link RootSchema} recorded on each parsed map, so pass the parsed value itself, not
 * a copy. Mirrors Rust's `Catalog::skipped`.
 */
export function skipped(root: Root): Skipped | undefined {
	const of = (map: object | undefined) => (map && SKIPPED.get(map)) ?? {};
	const sections = {
		video: of(root.video?.renditions),
		audio: of(root.audio?.renditions),
		text: of(root.text?.renditions),
		json: of(root.json?.tracks),
		binary: of(root.binary?.tracks),
		include: of(root.include),
	};
	const versions = Object.values(sections).flatMap((section) => Object.values(section));
	if (versions.length === 0) return undefined;
	return { version: Math.max(...versions), ...sections };
}

/**
 * The oldest moq-hang draft that reads an entry correctly given its ID, or undefined while that is
 * 3. Only a field an older reader would misread raises it; one it would merely ignore does not.
 */
function entryVersion(id: string, entry: { track?: string; warmup?: number }): number | undefined {
	// moq-hang-04: a reader that ignores `track` subscribes to the ID, a different track.
	if (track(id, entry) !== id) return 4;
	return undefined;
}

/** Video adds `warmup` to {@link entryVersion}. */
function videoVersion(id: string, entry: { track?: string; warmup?: number }): number | undefined {
	// moq-hang-04: a reader that ignores video `warmup` assumes every group starts at a keyframe,
	// and hands its decoder one that starts at a converging picture instead.
	if ((entry.warmup ?? 0) > 0) return 4;
	return entryVersion(id, entry);
}

/** Rewrite a map's entries with their computed `version` first, dropping any they carried. */
function versioned<T extends object>(
	entries: Record<string, T>,
	version: (id: string, entry: T) => number | undefined,
): Record<string, T> {
	return Object.fromEntries(
		Object.entries(entries).map(([id, entry]) => {
			const { version: _, ...rest } = entry as T & { version?: unknown };
			const computed = version(id, entry);
			return [id, computed === undefined ? rest : { version: computed, ...rest }];
		}),
	) as Record<string, T>;
}

/** A catalog update used one rendition ID for both a video and an audio rendition. */
export class DuplicateRendition extends Error {
	/** The rendition ID used for both video and audio. */
	readonly id: string;

	constructor(id: string) {
		super(`catalog rendition ID is both video and audio: ${id}`);
		this.name = "DuplicateRendition";
		this.id = id;
	}
}

/** Refuse an update with an ID naming both a video and an audio rendition. */
export function checkRenditions(root: Root): Root {
	const audio = root.audio?.renditions ?? {};
	const duplicate = Object.keys(root.video?.renditions ?? {}).find((id) => Object.hasOwn(audio, id));
	if (duplicate !== undefined) throw new DuplicateRendition(duplicate);
	return root;
}

/**
 * The catalog as a publisher writes it: refused as every reader refuses it, with each entry's
 * `version` computed rather than carried, so an edit can't claim less than an entry uses, and an
 * empty `media` left off the wire. No moq-hang-04 field reshapes the root, so the root `version` is
 * never written.
 *
 * Throws {@link DuplicateRendition}. Mirrors Rust's catalog serializer.
 */
export function encode(root: Root): Root {
	checkRenditions(root);
	const value = { ...root };
	delete value.version;
	if (value.media && value.media.clock === undefined && value.media.join === undefined) delete value.media;
	if (value.video) value.video = { ...value.video, renditions: versioned(value.video.renditions, videoVersion) };
	if (value.audio) value.audio = { ...value.audio, renditions: versioned(value.audio.renditions, entryVersion) };
	if (value.text) value.text = { ...value.text, renditions: versioned(value.text.renditions, entryVersion) };
	if (value.json) value.json = { ...value.json, tracks: versioned(value.json.tracks, entryVersion) };
	if (value.binary) value.binary = { ...value.binary, tracks: versioned(value.binary.tracks, entryVersion) };
	if (value.include) value.include = versioned(value.include, () => undefined);
	return value;
}

/** A catalog update carried a `broadcast` reference that walks above its reader's root. */
export class EscapingBroadcast extends Error {
	/** The offending reference, normalized. */
	readonly broadcast: RelativeBroadcast;

	constructor(broadcast: RelativeBroadcast) {
		super(`catalog broadcast reference escapes the root: ${broadcast}`);
		this.name = "EscapingBroadcast";
		this.broadcast = broadcast;
	}
}

/** Refuse an update with a `broadcast` reference that walks above the root from `base`. */
export function checkResolvable(root: Root, base: Moq.Path.Valid): Root {
	// Every section carrying a `broadcast` reference must be listed here; one left out
	// silently exempts its tracks from the check.
	const sections = [
		root.video?.renditions,
		root.audio?.renditions,
		root.text?.renditions,
		root.json?.tracks,
		root.binary?.tracks,
		root.include,
	];
	for (const section of sections) {
		for (const { broadcast } of Object.values(section ?? {})) {
			if (broadcast && Path.tryResolve(base, broadcast) === undefined) throw new EscapingBroadcast(broadcast);
		}
	}
	if (root.echo && Path.tryResolve(base, root.echo.path) === undefined) throw new EscapingBroadcast(root.echo.path);
	return root;
}

/**
 * Subscribe to a broadcast's catalog and iterate validated root updates.
 *
 * Throws {@link UnsupportedVersion}, {@link DuplicateRendition}, or {@link EscapingBroadcast} on an
 * update that fails validation. `broadcast` references are checked against the handle's `path` and
 * yielded unresolved.
 */
export async function* watch(broadcast: Moq.Broadcast.Consumer): AsyncIterable<Root> {
	const consumer = new Json.Snapshot.Consumer<Root>({
		track: broadcast.track(TRACK),
		schema: RootSchema,
		priority: PRIORITY.catalog,
	});
	try {
		for (;;) {
			const root = await consumer.latest();
			if (!root) return;
			yield checkResolvable(checkRenditions(root.value), broadcast.path);
		}
	} finally {
		consumer.close();
	}
}
