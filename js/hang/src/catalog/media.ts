import * as z from "zod/mini";
import { ClockSchema } from "./clock";
import { nonzeroU53Schema } from "./integers";

/**
 * How a viewer joins every audio and video track: the promise that each one starts a group at
 * least every `interval` outside a pause.
 *
 * `interval` is the longest GOP of any track, in whole milliseconds, and never zero. It is fixed for
 * the broadcast instance, since an HLS or DASH export cuts its segment grid on it and numbers every
 * rendition's segments from it. A broadcast without one is not HLS-renderable.
 */
export const JoinSchema = z.object({
	interval: nonzeroU53Schema,
});

/** How a viewer joins every audio and video track. */
export type Join = z.infer<typeof JoinSchema>;

/**
 * What applies to every audio and video track, at the catalog root's `media`.
 *
 * Omitted when it holds nothing. A malformed `clock` fails the catalog. A malformed `join` decodes
 * as absent, so it can never reject the media: the broadcast is merely not HLS-renderable.
 */
export const MediaSchema = z.object({
	// The broadcast's one continuous clock, if the publisher exposes one.
	clock: z.optional(ClockSchema),
	// Nested so a later join mechanism that is not a fixed grid can sit beside `interval`.
	join: z.catch(z.optional(JoinSchema), undefined),
});

/** What applies to every audio and video track: the broadcast's clock and join interval. */
export type Media = z.infer<typeof MediaSchema>;
