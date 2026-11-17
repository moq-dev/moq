import { Duration } from "@moq/clock";
import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import { Path } from "@moq/net";
import * as z from "zod/mini";
import { AudioSchema } from "./audio.ts";
import { BinarySchema } from "./binary.ts";
import { EchoSchema } from "./echo.ts";
import { TRACK } from "./format.ts";
import { IncludeSchema } from "./include.ts";
import { JsonSchema } from "./json.ts";
import type { RelativeBroadcast } from "./path.ts";
import { PRIORITY } from "./priority.ts";
import { renditions } from "./section.ts";
import { StatsSchema } from "./stats.ts";
import { TextSchema } from "./text.ts";
import { TimelineSchema } from "./timeline.ts";
import { track } from "./track.ts";
import { VideoSchema } from "./video.ts";

/** The newest moq-hang draft this package reads; a catalog whose `version` is higher is refused. */
export const VERSION = 4;

/** A catalog needs a newer moq-hang draft than this reader implements. */
export class UnsupportedVersion extends Error {
	/** The catalog version that was read. */
	readonly version: number;

	constructor(version: number) {
		super(`unsupported catalog version ${version}: upgrade to read it`);
		this.name = "UnsupportedVersion";
		this.version = version;
	}
}

// Thrown rather than reported as an issue, so it surfaces as itself and before any section, which
// the newer draft may have reshaped.
const VersionSchema = z.number().check(
	z.int(),
	z.nonnegative(),
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
 * present, or parsing refuses the whole catalog, the same as Rust. The `mpegts` section
 * (draft-lcurley-moq-mpegts), including a transport stream carried whole, is such an extension:
 * nothing in JS reads it yet.
 *
 * The root `version` is the oldest moq-hang draft that reads the catalog correctly, absent while
 * that is 3. Parsing throws {@link UnsupportedVersion} when it is above {@link VERSION}; a
 * publisher sets it with {@link encode}.
 */
export const RootSchema = z.looseObject({
	// First, so it is checked before any section.
	version: z.optional(VersionSchema),
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

/**
 * The oldest moq-hang draft that reads `root` correctly, or undefined while that is 3.
 *
 * Only a field an older reader would misread raises it; one it would merely ignore does not.
 */
function minVersion(root: Root): number | undefined {
	// moq-hang-04: a reader that ignores `track` subscribes to the ID, a different track.
	const renamed = [
		root.video?.renditions,
		root.audio?.renditions,
		root.text?.renditions,
		root.json?.tracks,
		root.binary?.tracks,
	].some((entries) => Object.entries(entries ?? {}).some(([id, config]) => track(id, config) !== id));

	return renamed ? 4 : undefined;
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
 * The catalog as a publisher writes it: refused as every reader refuses it, with the root
 * `version` computed rather than carried, so an edit can't claim less than the catalog uses, and an
 * empty `sync` left off the wire.
 *
 * Throws {@link DuplicateRendition}. Mirrors Rust's catalog serializer.
 */
export function encode(root: Root): Root {
	checkRenditions(root);
	const value = { ...root };
	delete value.version;
	if (value.sync && value.sync.clock === undefined && value.sync.interval === undefined) delete value.sync;
	const version = minVersion(root);
	if (version !== undefined) value.version = version;
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
