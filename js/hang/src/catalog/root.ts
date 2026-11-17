import { Duration } from "@moq/clock";
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
import type { RelativeBroadcast } from "./path";
import { PRIORITY } from "./priority";
import { renditions, SKIPPED } from "./section";
import { StatsSchema } from "./stats";
import { TextSchema } from "./text";
import { TimelineSchema } from "./timeline";
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
 * The media sections are `video`, `audio`, and `text`, alongside `sync`, the broadcast's timeline:
 * its `clock` (the one wall-clock mapping) and its join `interval`. `json` and `binary` list application data tracks that aren't media. A section is omitted
 * when it holds no tracks. `stats` names the publisher's media snapshot track, `echo`
 * solicits viewer feedback under a relative broadcast prefix, and `include` lists whole
 * broadcasts without opening them; each shares this catalog's PTS timeline and root settings.
 *
 * This is a *loose* object: unknown root sections pass through validation untouched, so an
 * application can add its own sections (e.g. `com.example.scte35`) without modifying hang. A base consumer
 * ignores the extra sections; an extended consumer validates them with its own schema, typically
 * built via `z.extend(RootSchema, { ... })`. An application names its own sections with a
 * namespaced key (`com.example.telemetry`): every section defined here must have its shape when
 * present, or parsing refuses the whole catalog, the same as Rust.
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
	// The broadcast's timeline, omitted when it holds nothing.
	sync: z.optional(TimelineSchema),
	stats: z.optional(StatsSchema),
	echo: z.optional(EchoSchema),
	text: z.optional(TextSchema),
	json: z.optional(JsonSchema),
	binary: z.optional(BinarySchema),
	include: z.optional(renditions(IncludeSchema, "include")),
});

/** The root catalog object: the media sections, `sync`, the data track sections, plus any app extensions. */
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
function entryVersion(id: string, entry: { track?: string }): number | undefined {
	// moq-hang-04: a reader that ignores `track` subscribes to the ID, a different track.
	if (track(id, entry) !== id) return 4;
	return undefined;
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
/**
 * The catalog as a publisher writes it: refused as every reader refuses it, with each entry's
 * `version` computed rather than carried, so an edit can't claim less than an entry uses, and an
 * empty `sync` left off the wire. No moq-hang-04 field reshapes the root, so the root `version` is
 * never written.
 *
 * Throws {@link DuplicateRendition}. Mirrors Rust's catalog serializer.
 */
export function encode(root: Root): Root {
	checkRenditions(root);
	const value = { ...root };
	delete value.version;
	if (value.sync && value.sync.clock === undefined && value.sync.interval === undefined) delete value.sync;
	if (value.video) value.video = { ...value.video, renditions: versioned(value.video.renditions, entryVersion) };
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

/** Options for {@link watch}. */
export type WatchConfig = {
	/** The caller's playout delay behind the live edge; omitted reads only the newest catalog. */
	delay?: Duration;
};

/**
 * Subscribe to a broadcast's catalog and iterate validated root updates, each with its timestamp.
 *
 * An update takes effect at `at` on the broadcast's media timeline, so a caller holds one it reads
 * before its media reaches `at`, and applies one read late, or the first it reads, at once. On a
 * scale-less catalog track `at` is a group key with no time, so the update applies on arrival.
 *
 * Without a `delay`, it reads only the newest catalog, for a caller that applies each on arrival. A
 * caller that holds updates passes its playout delay and reads, in order, every update it may still
 * need: one stays deliverable until it falls `delay` behind the live edge.
 *
 * Throws {@link UnsupportedVersion}, {@link DuplicateRendition}, or {@link EscapingBroadcast} on an
 * update that fails validation. `broadcast` references are checked against the handle's `path` and
 * yielded unresolved.
 */
export async function* watch(broadcast: Moq.Broadcast.Consumer, config?: WatchConfig): AsyncIterable<Moq.Timed<Root>> {
	const delay = config?.delay ?? Duration.ZERO;
	const holds = !delay.isZero();
	const consumer = new Json.Snapshot.Consumer<Root>({
		track: broadcast.track(TRACK),
		schema: RootSchema,
		priority: PRIORITY.catalog,
		delay,
	});
	try {
		for (;;) {
			const next = await (holds ? consumer.next() : consumer.latest());
			if (!next) return;
			yield { value: checkResolvable(checkRenditions(next.value), broadcast.path), at: next.at };
		}
	} finally {
		consumer.close();
	}
}
