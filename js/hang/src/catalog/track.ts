import { z } from "zod";

export const TrackSchema = z.object({
	name: z.string(),
	// TODO: Default is for backwards compatibility with old catalogs
	priority: z.number().int().min(0).max(255).default(0),
});
export type Track = z.infer<typeof TrackSchema>;
