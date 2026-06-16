import * as z from "zod/mini";

import { AudioSchema } from "./audio";
import { DataSchema } from "./data";
import { VideoSchema } from "./video";

/**
 * The root catalog: the base media sections every hang broadcast carries.
 *
 * This is a *loose* object: unknown root sections pass through validation untouched, so an
 * application can add its own sections (e.g. `scte35`) without modifying hang. A base consumer
 * ignores the extra sections; an extended consumer validates them with its own schema, typically
 * built via `z.extend(RootSchema, { ... })`.
 *
 * The `data` section lists arbitrary application-defined tracks (e.g. a `meta.json` track) carried
 * alongside the media within the same broadcast.
 */
export const RootSchema = z.looseObject({
	video: z.optional(VideoSchema),
	audio: z.optional(AudioSchema),
	data: z.optional(DataSchema),
});

/**
 * The root catalog object, with optional `video`, `audio`, and `data` (custom application tracks)
 * sections plus any app extensions.
 */
export type Root = z.infer<typeof RootSchema>;
