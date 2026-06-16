import * as z from "zod/mini";

import { AudioSchema } from "./audio";
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
});

/** The root catalog object, with optional video and audio sections plus any app extensions. */
export type Root = z.infer<typeof RootSchema>;
