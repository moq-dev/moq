import * as z from "zod/mini";

import { AudioSchema } from "./audio";
import { TextSchema } from "./text";
import { VideoSchema } from "./video";

/**
 * The root catalog: the base media sections every hang broadcast carries.
 *
 * This is a *loose* object: unknown root sections pass through validation untouched, so an
 * application can add its own sections (e.g. `scte35`) without modifying hang. A base consumer
 * ignores the extra sections; an extended consumer validates them with its own schema, typically
 * built via `z.extend(RootSchema, { ... })`.
 */
export const RootSchema = z.looseObject({
	video: z.optional(VideoSchema),
	audio: z.optional(AudioSchema),
	// `text` is now a reserved media section, but a catalog that carried an unrelated `text` key
	// before this existed must not fail to parse: fall back to `undefined` (dropping the section)
	// rather than rejecting the whole catalog, so video/audio still play.
	text: z.catch(z.optional(TextSchema), undefined),
});

/** The root catalog object, with optional video and audio sections plus any app extensions. */
export type Root = z.infer<typeof RootSchema>;
