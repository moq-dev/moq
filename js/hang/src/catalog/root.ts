import * as z from "@zod/mini";

import { ArchiveSchema } from "./archive";
import { AudioSchema } from "./audio";
import { BinarySchema } from "./binary";
import { JsonSchema } from "./json";
import { section } from "./section";
import { TextSchema } from "./text";
import { VideoSchema } from "./video";

/**
 * The root catalog: the base sections every hang broadcast carries.
 *
 * The media sections are `video`, `audio`, and `text`, alongside the broadcast's `archive`
 * (the segment index and any durable recording); `json` and `binary` list application data
 * tracks that aren't media. A section is omitted when it holds no tracks.
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
	// `text` is now a reserved media section, but a catalog that carried an unrelated `text` key
	// before this existed must not fail to parse: fall back to `undefined` (dropping the section)
	// rather than rejecting the whole catalog, so video/audio still play.
	text: z.catch(z.optional(TextSchema), undefined),
	// Lenient for the same reason as `text` above: `json` and `binary` are generic enough keys that
	// an application could have been carrying its own before these sections were reserved. Narrower
	// than `text`'s blanket catch, though, because these entries have a required field: a value that
	// IS a section but carries a mode-less track still fails, rather than silently dropping every
	// data track. See `section`.
	json: section(JsonSchema, "tracks"),
	binary: section(BinarySchema, "tracks"),
});

/** The root catalog object: the media and archive sections, the data track sections, plus any app extensions. */
export type Root = z.infer<typeof RootSchema>;
