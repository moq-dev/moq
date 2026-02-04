import { z } from "zod";

export const TrackSchema = z.object({
	name: z.string(),
});
export type Track = z.infer<typeof TrackSchema>;
