import * as z from "zod/mini";

/** Schema for a catalog track reference, identified by name. */
export const TrackSchema = z.object({
	name: z.string(),
});
/** A catalog track reference, identified by name. */
export type Track = z.infer<typeof TrackSchema>;
