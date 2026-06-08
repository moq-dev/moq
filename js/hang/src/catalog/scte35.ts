import * as z from "zod/mini";

// A single splice event (ad break) signalled via SCTE-35.
export const SpliceSchema = z.object({
	// splice_event_id from the SCTE-35 message.
	id: z.number().check(z.int(), z.nonnegative()),

	// Presentation time of the splice point, in seconds. Omitted for an immediate splice.
	startTime: z.optional(z.number().check(z.nonnegative())),

	// Duration of the break in seconds, when known. Omitted for a cancel or open-ended break.
	duration: z.optional(z.number().check(z.nonnegative())),

	// True at the start of a break (out of network), false on return.
	out: z.optional(z.boolean()),
});

/**
 * SCTE-35 signaling: ad-insertion / splice markers.
 *
 * An optional catalog extension, kept out of the base {@link RootSchema} so the base catalog stays
 * generic. An application opts in by extending the root schema, then publishes and reads the
 * section through the shared catalog producer/consumer:
 *
 * ```ts
 * const Schema = z.extend(Catalog.RootSchema, { scte35: z.optional(Catalog.Scte35Schema) });
 * ```
 *
 * The contents are intentionally minimal; it exists to demonstrate (and test) extending the
 * catalog without modifying hang.
 */
export const Scte35Schema = z.object({
	// Currently active splice events.
	splices: z.optional(z.array(SpliceSchema)),
});

export type Splice = z.infer<typeof SpliceSchema>;
export type Scte35 = z.infer<typeof Scte35Schema>;
