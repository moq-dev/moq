import * as z from "zod/mini";

export const PreviewSchema = z.object({
	name: z.optional(z.string()), // name
	avatar: z.optional(z.string()), // avatar

	audio: z.optional(z.boolean()), // audio enabled
	video: z.optional(z.boolean()), // video enabled

	typing: z.optional(z.boolean()), // actively typing
	chat: z.optional(z.boolean()), // chatted recently
	screen: z.optional(z.boolean()), // screen sharing
});

export type Preview = z.infer<typeof PreviewSchema>;
