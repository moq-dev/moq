import * as z from "zod/mini";

/** The container kinds this build knows how to decode. */
const KNOWN_KINDS = ["legacy", "cmaf", "loc"];

/**
 * A container this build does not recognize, preserved verbatim.
 *
 * The original object is kept verbatim under `raw`, so nothing about the rendition is lost and a
 * republisher can write it back out unchanged. Such a rendition must be ignored, not decoded.
 *
 * Recognized kinds are rejected here so they can only ever parse through their own strict
 * schema. Without that, a malformed known container (`{"kind":"cmaf"}` with no `init`) would
 * fall through to this arm, still report as CMAF, and hand decoders an undefined init segment.
 */
export const UnknownContainerSchema = z.pipe(
	z.looseObject({
		kind: z.string().check(
			z.refine((kind) => !KNOWN_KINDS.includes(kind), {
				message: "recognized container kind must match its own schema",
			}),
		),
	}),
	// Map to a literal `kind` so {@link Container} stays a discriminated union: a bare
	// `kind: string` arm would widen the discriminant and stop `kind === "cmaf"` from
	// narrowing. `raw` keeps the original object, including its real `kind`.
	z.transform((raw) => ({ kind: "unknown" as const, raw })),
);

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

/**
 * The per-frame container format declared in the catalog.
 *
 * A discriminated union: `container.kind === "cmaf"` narrows and gives you `init`. An
 * unrecognized container arrives as `{ kind: "unknown", raw }` rather than widening `kind`,
 * so tolerating a future container costs no type safety here.
 */
export type Container = z.infer<typeof ContainerSchema>;

/** The CMAF variant of {@link Container}, carrying the base64 init segment. */
export type CmafContainer = Extract<Container, { kind: "cmaf" }>;

/** Whether a container can be decoded by this build, i.e. its `kind` is recognized. */
export function containerSupported(container: Container): boolean {
	return container.kind !== "unknown";
}
