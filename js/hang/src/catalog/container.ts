import * as z from "zod/mini";

/** The container kinds this build knows how to decode. */
const KNOWN_KINDS = ["legacy", "cmaf", "loc"];

/**
 * A container this build does not recognize, preserved verbatim.
 *
 * Kept intact so reparsing and republishing a catalog round-trips the rendition instead of
 * corrupting it. Such a rendition must be ignored rather than decoded.
 */
export const UnknownContainerSchema = z.looseObject({
	kind: z.string(),
});

/**
 * Container format for frame timestamp encoding and frame payload structure.
 *
 * - "legacy": QUIC VarInt timestamp prefix followed by the raw codec payload.
 *             Timestamps are in microseconds.
 * - "cmaf": Fragmented MP4 container - frames contain complete moof+mdat fragments.
 *           The init segment (ftyp+moov) is base64-encoded in the catalog.
 * - "loc": Low Overhead Container (draft-ietf-moq-loc). Each frame has a small
 *          property block followed by the codec payload.
 *
 * Anything else parses as {@link UnknownContainerSchema} instead of throwing, so one rendition
 * using a future container does not take down the rest of the catalog.
 */
export const ContainerSchema = z._default(
	z.union([
		z.discriminatedUnion("kind", [
			// The default hang container
			z.object({ kind: z.literal("legacy") }),
			// CMAF container with base64-encoded init segment (ftyp+moov).
			z.object({
				kind: z.literal("cmaf"),
				init: z.base64(),
			}),
			// Low Overhead Container.
			z.object({ kind: z.literal("loc") }),
		]),
		UnknownContainerSchema,
	]),
	{ kind: "legacy" },
);

/** The per-frame container format declared in the catalog. */
export type Container = z.infer<typeof ContainerSchema>;

/** The CMAF variant of {@link Container}, carrying the base64 init segment. */
export type CmafContainer = Extract<Container, { kind: "cmaf" }>;

/**
 * Whether the container is CMAF, narrowing it so `init` is available.
 *
 * The passthrough case makes `kind` a plain string, so an equality check alone no longer
 * narrows the union.
 */
export function isCmafContainer(container: Container): container is CmafContainer {
	return container.kind === "cmaf";
}

/** Whether a container can be decoded by this build, i.e. its `kind` is recognized. */
export function containerSupported(container: Container): boolean {
	return KNOWN_KINDS.includes(container.kind);
}
