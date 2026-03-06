import { z } from "zod";

/**
 * Container format for frame timestamp encoding and frame payload structure.
 *
 * - "legacy": Uses QUIC VarInt encoding (1-8 bytes, variable length), raw frame payloads.
 *             Timestamps are in microseconds.
 * - "cmaf": Fragmented MP4 container - frames contain complete moof+mdat fragments.
 *           The init segment (ftyp+moov) is base64-encoded in the catalog.
 */
export const ContainerSchema = z
	.discriminatedUnion("kind", [
		// The default hang container
		z.object({ kind: z.literal("legacy") }),
		// CMAF container with base64-encoded init segment (ftyp+moov)
		z.object({
			kind: z.literal("cmaf"),
			// Base64-encoded init segment (ftyp+moov)
			initData: z.string(),
		}),
	])
	.default({ kind: "legacy" });

export type Container = z.infer<typeof ContainerSchema>;
