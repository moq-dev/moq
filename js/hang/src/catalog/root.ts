import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import * as z from "@zod/mini";
import { ArchiveSchema } from "./archive";
import { AudioSchema } from "./audio";
import { BinarySchema } from "./binary";
import { ClockSchema } from "./clock";
import { TRACK } from "./format";
import { JsonSchema } from "./json";
import { PRIORITY } from "./priority";
import { section } from "./section";
import { TextSchema } from "./text";
import { VideoSchema } from "./video";

/**
 * The root catalog: the base sections every hang broadcast carries.
 *
 * The media sections are `video`, `audio`, and `text`, alongside the broadcast's `archive`
 * (the segment index and any durable recording) and its `clock` (the one wall-clock mapping);
 * `json` and `binary` list application data tracks that aren't media. A section is omitted
 * when it holds no tracks.
 *
 * This is a *loose* object: unknown root sections pass through validation untouched, so an
 * application can add its own sections (e.g. `scte35`) without modifying hang. A base consumer
 * ignores the extra sections; an extended consumer validates them with its own schema, typically
 * built via `z.extend(RootSchema, { ... })`.
 */
export const RootSchema = z.looseObject({
	video: z.optional(VideoSchema),
	audio: z.optional(AudioSchema),
	// The broadcast's segment index and any durable archive, if the publisher offers one.
	archive: z.optional(ArchiveSchema),
	// The broadcast's one continuous clock, if the publisher exposes one. Independent of
	// `archive`: a live-only publisher exposes its mapping without creating a segment index.
	clock: z.optional(ClockSchema),
	// `text`, `json`, and `binary` are generic enough keys that an application could have been
	// carrying its own before these sections were reserved, so a value that isn't a section decodes
	// as absent rather than failing the whole catalog. A value that IS a section but carries a
	// malformed entry (a rendition with no `format`, a track with no `mode`) still fails, the same
	// as Rust, rather than silently dropping every caption or data track. See `section`.
	text: section(TextSchema, "renditions"),
	json: section(JsonSchema, "tracks"),
	binary: section(BinarySchema, "tracks"),
});

/** The root catalog object: the media and archive sections, the data track sections, plus any app extensions. */
export type Root = z.infer<typeof RootSchema>;

/** Maximum number of video, audio, and text renditions accepted in one catalog update. */
export const MAX_RENDITIONS = 64;

/** A catalog update announced more media renditions than a reader will retain. */
export class TooManyRenditions extends Error {
	readonly count: number;

	constructor(count: number) {
		super(`catalog has ${count} renditions, over the limit of ${MAX_RENDITIONS}`);
		this.name = "TooManyRenditions";
		this.count = count;
	}
}

/** Refuse an update with too many media renditions. */
export function checkRenditions(root: Root): Root {
	const count =
		Object.keys(root.video?.renditions ?? {}).length +
		Object.keys(root.audio?.renditions ?? {}).length +
		Object.keys(root.text?.renditions ?? {}).length;
	if (count > MAX_RENDITIONS) throw new TooManyRenditions(count);
	return root;
}

/** Subscribe to a broadcast's catalog and iterate validated root updates. */
export async function* watch(broadcast: Moq.Broadcast.Consumer): AsyncIterable<Root> {
	const track = broadcast.track(TRACK).subscribe({ priority: PRIORITY.catalog });
	try {
		const consumer = new Json.Snapshot.Consumer<Root>({ track, schema: RootSchema });
		for await (const root of consumer) yield checkRenditions(root);
	} finally {
		track.close();
	}
}
