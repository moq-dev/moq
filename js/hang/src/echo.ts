/**
 * Media report schemas with cumulative counters and optional gauges.
 * Missing counters mean zero; parsing preserves omission.
 * @module
 */

import type { Path } from "@moq/net";
import * as z from "@zod/mini";
import { u53Schema } from "./catalog/integers";
import { measurementSchema } from "./snapshot";
import { TransportSchema } from "./stats";

/** The newest received media timestamp paired with its arrival wall time. */
export const ArrivalSchema = z.object({
	// Media timestamp in milliseconds.
	timestamp: u53Schema,
	// Arrival milliseconds since the MoQ epoch.
	wall: u53Schema,
});
/** The newest received media timestamp and its wall-clock arrival. */
export type Arrival = z.infer<typeof ArrivalSchema>;

/** Cumulative counters and latest playout measurements for one rendition. */
export const RenditionSchema = z.object({
	// Cumulative media frames received.
	frames: z.optional(u53Schema),
	// Cumulative media payload bytes received.
	bytes: z.optional(u53Schema),
	// Cumulative media frames decoded.
	decoded: z.optional(u53Schema),
	// Cumulative frames received too late for playout.
	late: z.optional(u53Schema),
	// Cumulative decode errors.
	errors: z.optional(u53Schema),
	// Cumulative playout stalls.
	stalls: z.optional(u53Schema),
	// Cumulative stalled duration in milliseconds.
	stalled: z.optional(u53Schema),
	// Cumulative audio buffer underruns.
	underruns: z.optional(u53Schema),
	// The newest received media timestamp and its arrival wall time.
	arrival: z.optional(
		z.pipe(
			z.nullable(ArrivalSchema),
			z.transform((value) => value ?? undefined),
		),
	),
	// Latest playout latency in milliseconds; absent when unavailable.
	latency: measurementSchema,
});
/** One rendition's delivery counters and latest playout measurements. */
export type Rendition = z.infer<typeof RenditionSchema>;
/** One cumulative viewer report, keyed by the catalog's rendition IDs. */
export const SnapshotSchema = z.looseObject({
	// Connection measurements; unavailable fields remain absent.
	transport: z.optional(TransportSchema),
	// Empty entries retain a rendition ID whose counters are all zero.
	renditions: z.optional(z.record(z.string(), RenditionSchema)),
});
/** One viewer report, with application or container sections flattened beside it. */
export type Snapshot = z.infer<typeof SnapshotSchema>;

/** The suffix identifying viewer feedback broadcasts at announce time. */
export const SUFFIX = ".echo";
/** The snapshot track carried by an echo broadcast. */
export const TRACK = "feedback.json";
/** Whether a broadcast path identifies feedback rather than playable media. */
export function isEcho(path: Path.Valid): boolean {
	return path.endsWith(SUFFIX);
}
