import * as z from "@zod/mini";
import { RelativeBroadcastSchema } from "./path";

/** The relative prefix under which viewers may publish feedback. */
export const EchoSchema = z.object({
	// The prefix relative to the broadcast serving this catalog.
	path: RelativeBroadcastSchema,
});

/** The relative prefix soliciting viewer feedback. */
export type Echo = z.infer<typeof EchoSchema>;
