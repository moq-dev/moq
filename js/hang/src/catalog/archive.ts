import * as z from "@zod/mini";
import { nonzeroU53Schema, u53, u53Schema } from "./integers";
import { RelativeBroadcastSchema } from "./path";

/** The moq epoch (2020-01-01T00:00:00Z) in Unix-epoch milliseconds. */
export const MOQ_EPOCH_UNIX_MILLIS = 1_577_836_800_000;

/** A nonzero Rust `u32`, used by the archive timescale. */
const timescaleSchema = nonzeroU53Schema.check(z.lte(4_294_967_295));

/**
 * The recording object format advertised in {@link Archive.version} when a store is present.
 */
export const ARCHIVE_VERSION = 1;

/**
 * Discovers the broadcast's segment index and any durable archive.
 *
 * This is the catalog's one name for the segment index: a live publisher sets the
 * timeline fields (`track`, `timescale`, `durationMax`) alone, and a recording
 * also names the replay broadcast and object store those ranges live under. Every
 * advertised range is FETCHable; with a store they are durable. There is no sibling
 * `timeline` entry. Wall-clock mapping lives at the catalog root (`clock`), not here.
 */
export const ArchiveSchema = z.object({
	// The name of the MoQ track carrying the broadcast's segment records.
	track: z.string(),

	// Units per second for the records' timestamps. Defaults to milliseconds.
	timescale: z._default(timescaleSchema, u53(1000)),

	// The declared upper bound on a segment's duration in timescale units.
	durationMax: z.optional(u53Schema),

	// The MoQ broadcast the archive is served back from, relative to this catalog, if any.
	// Absent when the timeline lives on this broadcast. A wildcard replay path names no
	// generation: compare this path and `store` to tell recordings apart.
	replay: z.optional(RelativeBroadcastSchema),

	// The object-store URL the recording objects live under, if the publisher exposes one.
	store: z.optional(z.url()),

	// The recording object format version, {@link ARCHIVE_VERSION} when this package writes
	// objects. Absent when there is no store.
	version: z.optional(u53Schema),
});

/** The catalog's root archive entry: the timeline track plus optional replay, store, and version. */
export type Archive = z.infer<typeof ArchiveSchema>;
