import * as z from "zod/mini";

export const TrackSchema = z.object({
	name: z.string(),
});
export type Track = z.infer<typeof TrackSchema>;
