import * as z from "zod/mini";

import { AudioSchema } from "./audio";
import { BinarySchema } from "./binary";
import { JsonSchema } from "./json";
import { VideoSchema } from "./video";

/**
 * The root catalog: the base sections every hang broadcast carries.
 *
 * The media sections are `video` and `audio`; `json` and `binary` list application data tracks
 * that aren't media. A section is omitted when it holds no tracks.
 *
 * This is a *loose* object: unknown root sections pass through validation untouched, so an
 * application can add its own sections (e.g. `scte35`) without modifying hang. A base consumer
 * ignores the extra sections; an extended consumer validates them with its own schema, typically
 * built via `z.extend(RootSchema, { ... })`.
 */
export const RootSchema = z.looseObject({
	video: z.optional(VideoSchema),
	audio: z.optional(AudioSchema),
	json: z.optional(JsonSchema),
	binary: z.optional(BinarySchema),
});

/** The root catalog object: the media sections, the data track sections, plus any app extensions. */
export type Root = z.infer<typeof RootSchema>;
