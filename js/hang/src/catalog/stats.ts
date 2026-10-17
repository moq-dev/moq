import * as z from "@zod/mini";

/** The publisher's optional media snapshot track in this broadcast. */
export const StatsSchema = z.object({
	// The publisher's snapshot track name.
	track: z.string(),
});

/** The publisher's media snapshot track in this broadcast. */
export type Stats = z.infer<typeof StatsSchema>;
