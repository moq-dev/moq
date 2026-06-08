import * as z from "zod/mini";

import { AudioSchema } from "./audio";
import { VideoSchema } from "./video";

// The base catalog: just the media tracks every hang broadcast carries.
//
// Applications layer their own sections on top with `z.extend`, e.g.
//
//   const MyRoot = z.extend(RootSchema, { scte35: z.optional(Scte35Schema) });
//
// and feed that schema to `@moq/json`'s Producer/Consumer to publish and subscribe with
// the same snapshot/delta semantics and validation as the base catalog. App-specific sections
// (chat, user, location, ...) live in the application layer, not here.
export const RootSchema = z.object({
	video: z.optional(VideoSchema),
	audio: z.optional(AudioSchema),
});

export type Root = z.infer<typeof RootSchema>;
