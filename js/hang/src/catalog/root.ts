import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import { Path, Track } from "@moq/net";
import * as z from "@zod/mini";
import { AudioSchema } from "./audio";
import { BinarySchema } from "./binary";
import { ClockSchema } from "./clock";
import { ContributionSchema } from "./contribution";
import { EchoSchema } from "./echo";
import { TRACK } from "./format";
import { JsonSchema } from "./json";
import type { RelativeBroadcast } from "./path";
import { PRIORITY } from "./priority";
import { named, section } from "./section";
import { StatsSchema } from "./stats";
import { TextSchema } from "./text";
import { track } from "./track";
import { VideoSchema } from "./video";

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
 * The media sections are `video`, `audio`, and `text`, alongside the broadcast's `clock` (the
 * one wall-clock mapping);
 * `json` and `binary` list application data tracks that aren't media. A section is omitted
 * when it holds no tracks. `stats` names the publisher's media snapshot track, `echo`
 * solicits viewer feedback under a relative broadcast prefix, and `contributions` lists whole
 * broadcasts without opening them.
 *
 * This is a *loose* object: unknown root sections pass through validation untouched, so an
 * application can add its own sections (e.g. `scte35`) without modifying hang. A base consumer
 * ignores the extra sections; an extended consumer validates them with its own schema, typically
 * built via `z.extend(RootSchema, { ... })`.
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
	// The broadcast's one continuous clock, if the publisher exposes one.
	clock: z.optional(ClockSchema),
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
	contributions: z.optional(z.record(z.string(), ContributionSchema)),
});

/** The root catalog object: the media sections, the clock, the data track sections, plus any app extensions. */
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

	// moq-hang-04: a reader that ignores video `warmup` assumes every group starts at a keyframe,
	// and hands its decoder one that starts at a converging picture instead.
	const warmup = Object.values(root.video?.renditions ?? {}).some((config) => (config.warmup ?? 0) > 0);

	return renamed || warmup ? 4 : undefined;
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
 * `version` computed rather than carried, so an edit can't claim less than the catalog uses.
 *
 * Throws {@link DuplicateRendition}. Mirrors Rust's catalog serializer.
 */
export function encode(root: Root): Root {
	checkRenditions(root);
	const value = { ...root };
	delete value.version;
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
		root.contributions,
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
	const track = broadcast.track(TRACK).subscribe({ ...Track.Subscription.default(), priority: PRIORITY.catalog });
	try {
		const consumer = new Json.Snapshot.Consumer<Root>({ track, schema: RootSchema });
		for (;;) {
			const root = await consumer.latest();
			if (!root) return;
			yield checkResolvable(checkRenditions(root.value), broadcast.path);
		}
	} finally {
		track.close();
	}
}
