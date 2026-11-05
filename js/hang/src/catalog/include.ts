import * as z from "zod/mini";
import { InstanceSchema, RelativeBroadcastSchema } from "./path";

/**
 * A whole broadcast whose own catalog describes its renditions, listed without opening it.
 *
 * It shares the listing catalog's PTS timeline and root settings, including `media.clock` and
 * `media.join.interval`; its own catalog's settings are ignored.
 *
 * Listing one subscribes to nothing: a consumer opens the include's catalog only when it
 * selects it, including an HLS master listing its renditions. An on-demand publisher answers
 * the catalog without starting media. Its rendition IDs belong to that broadcast, never the
 * listing catalog.
 */
export const IncludeSchema = z.object({
	// The include's broadcast, relative to the broadcast serving this catalog.
	broadcast: RelativeBroadcastSchema,
	// The stream the reference names, with a rendition `instance`'s meaning. A composer leaves it
	// out until the include resolves.
	instance: z.optional(InstanceSchema),
});

/** A whole broadcast listed for explicit, lazy selection. */
export type Include = z.infer<typeof IncludeSchema>;
