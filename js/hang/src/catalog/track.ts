import * as z from "@zod/mini";

/** Schema for a catalog track reference, identified by name. */
export const TrackSchema = z.object({
	name: z.string(),
});
/** A catalog track reference, identified by name. */
export type Track = z.infer<typeof TrackSchema>;

/**
 * The name of an entry's track in its broadcast: its `track` when set, otherwise its ID (its key
 * in the catalog). Every reader subscribes through this so the two never diverge.
 */
export function track(id: string, config: { track?: string }): string {
	return config.track ?? id;
}
