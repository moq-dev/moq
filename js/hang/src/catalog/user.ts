import * as z from "zod/mini";

export const UserSchema = z.object({
	id: z.optional(z.string()),
	name: z.optional(z.string()),
	avatar: z.optional(z.string()), // TODO allow using a track instead of a URL?
	color: z.optional(z.string()),
});

export type User = z.infer<typeof UserSchema>;
