import * as z from "zod/mini";
import { RelativeBroadcastSchema } from "./path.ts";

/**
 * A whole broadcast whose own catalog describes its renditions, listed without opening it.
 *
 * It shares the listing catalog's PTS timeline and root settings, including `sync.clock` and
 * `sync.interval`; its own catalog's settings are ignored.
 *
 * Listing one subscribes to nothing: a consumer opens the include's catalog only when it
 * selects it, including an HLS master listing its renditions. An on-demand publisher answers
 * the catalog without starting media. Its rendition IDs belong to that broadcast, never the
 * listing catalog.
 */
export const IncludeSchema = z.object({
	// The include's broadcast, relative to the broadcast serving this catalog.
	broadcast: RelativeBroadcastSchema,
});

/** A whole broadcast listed for explicit, lazy selection. */
export type Include = z.infer<typeof IncludeSchema>;
