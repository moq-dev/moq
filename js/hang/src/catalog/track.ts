import { z } from "zod";

export const TrackSchema = z.object({
	name: z.string(),
	// DEPRECATED: The priority of the track, relative to other tracks in the broadcast.
	// The subscriber is supposed to choose its own priority, instead of being told.
	priority: z.number().int().min(0).max(255).default(0),
});
export type Track = z.infer<typeof TrackSchema>;
