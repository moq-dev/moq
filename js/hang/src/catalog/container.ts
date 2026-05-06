import * as z from "zod/mini";
import { u53, u53Schema } from "./integers";

/**
 * Container format for frame timestamp encoding and frame payload structure.
 *
 * - "legacy": QUIC VarInt timestamp prefix followed by the raw codec payload.
 *             Timestamps are in microseconds.
 * - "cmaf": Fragmented MP4 container - frames contain complete moof+mdat fragments.
 *           The init segment (ftyp+moov) is base64-encoded in the catalog.
 * - "loc": Low Overhead Container (draft-ietf-moq-loc). Each frame has a small
 *          property block followed by the codec payload. The catalog-level
 *          `timescale` (units per second) is the fallback used when a frame
 *          omits its 0x08 timescale property. Defaults to 1_000_000 (microseconds).
 */
export const ContainerSchema = z._default(
	z.discriminatedUnion("kind", [
		// The default hang container
		z.object({ kind: z.literal("legacy") }),
		// CMAF container with base64-encoded init segment (ftyp+moov)
		z.object({
			kind: z.literal("cmaf"),
			// Base64-encoded init segment (ftyp+moov)
			init: z.base64(),
		}),
		// Low Overhead Container with optional fallback timescale (units/sec).
		z.object({
			kind: z.literal("loc"),
			timescale: z._default(u53Schema, u53(1_000_000)),
		}),
	]),
	{ kind: "legacy" },
);

export type Container = z.infer<typeof ContainerSchema>;
