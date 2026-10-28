/**
 * Media report schemas with cumulative counters and optional gauges.
 * Missing counters mean zero; parsing preserves omission.
 * @module
 */
import * as z from "@zod/mini";
import { u53Schema } from "./catalog/integers";
import { measurementSchema } from "./snapshot";
/** Transport gauges and cumulative loss counters known to the observer. */
export const TransportSchema = z.object({
	// Latest round-trip time in milliseconds.
	rtt: measurementSchema,
	// Latest estimated connection bitrate in bits per second.
	rate: measurementSchema,
	// Cumulative bytes reported lost by the transport.
	lostBytes: measurementSchema,
	// Cumulative packets reported lost by the transport.
	lostPackets: measurementSchema,
	// Transport sample age in milliseconds at reporting time.
	age: measurementSchema,
});
/** Transport measurements, absent when unavailable; a measured zero remains zero. */
export type Transport = z.infer<typeof TransportSchema>;
/** Cumulative counters and target bitrate for one locally published rendition. */
export const RenditionSchema = z.object({
	// Cumulative media frames sent.
	frames: z.optional(u53Schema),
	// Cumulative media payload bytes sent.
	bytes: z.optional(u53Schema),
	// Cumulative keyframes sent.
	keyframes: z.optional(u53Schema),
	// Cumulative frames skipped before sending.
	skipped: z.optional(u53Schema),
	// Latest target encoder bitrate in bits per second; absent when unavailable.
	bitrate: measurementSchema,
});
/** One locally published rendition's counters and optional target bitrate. */
export type Rendition = z.infer<typeof RenditionSchema>;
/** One cumulative publisher report, keyed by the catalog's rendition IDs. */
export const SnapshotSchema = z.looseObject({
	// Connection measurements; unavailable fields remain absent.
	transport: z.optional(TransportSchema),
	// Empty entries retain a rendition ID whose counters are all zero.
	renditions: z.optional(z.record(z.string(), RenditionSchema)),
});
/** One publisher report, with application or container sections flattened beside it. */
export type Snapshot = z.infer<typeof SnapshotSchema>;
