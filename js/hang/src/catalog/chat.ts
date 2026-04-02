import * as z from "zod/mini";
import { TrackSchema } from "./track";

export const ChatSchema = z.object({
	message: z.optional(TrackSchema),
	typing: z.optional(TrackSchema),
});

export type Chat = z.infer<typeof ChatSchema>;
