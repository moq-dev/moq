import * as z from "zod/mini";
import { ClockSchema } from "./clock";
import { nonzeroU53Schema } from "./integers";

/**
 * The broadcast's timeline, at the catalog root's `sync`: the wall-clock time of PTS zero and how
 * often a viewer can join.
 *
 * Omitted when it holds nothing. A malformed `clock` or `interval` fails the catalog.
 */
export const SyncSchema = z.object({
	// The broadcast's one continuous clock, if the publisher exposes one.
	clock: z.optional(ClockSchema),
	// The promise that every audio and video track starts a group at least this often outside a
	// pause: the longest GOP of any track, in whole milliseconds, and never zero. Fixed for the
	// broadcast instance, since an HLS or DASH export cuts its segment grid on it and numbers every
	// rendition's segments from it. A broadcast without one is not HLS-renderable.
	interval: z.optional(nonzeroU53Schema),
});

/** The broadcast's timeline: its clock and join interval. */
export type Sync = z.infer<typeof SyncSchema>;
