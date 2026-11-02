import * as z from "@zod/mini";
import { EpochSchema, RelativeBroadcastSchema } from "./path";

/**
 * A whole broadcast whose own catalog describes its renditions, listed without opening it.
 *
 * Listing one subscribes to nothing: a consumer opens the contribution's catalog only when it
 * selects one of its renditions. Its rendition IDs belong to that broadcast, never the listing
 * catalog.
 */
export const ContributionSchema = z.object({
	// The contribution's broadcast, relative to the broadcast serving this catalog.
	broadcast: RelativeBroadcastSchema,
	// The publisher instance the reference names, with a rendition `epoch`'s meaning. A composer
	// leaves it out until the contribution resolves.
	epoch: z.optional(EpochSchema),
});

/** A whole broadcast listed for explicit, lazy selection. */
export type Contribution = z.infer<typeof ContributionSchema>;
