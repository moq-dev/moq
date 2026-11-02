import * as z from "zod/mini";

/**
 * A hex-encoded byte string, the catalog's encoding for WebCodecs `Uint8Array` fields.
 *
 * Validated on both publish and consume so a wrongly encoded value (base64, the encoding the CMAF
 * `init` field uses, is the easy mistake) fails against the publisher that produced it instead of
 * against every consumer.
 */
export const hexSchema = z.string().check(z.regex(/^([0-9a-fA-F]{2})*$/, "expected hex"));

/** A hex-encoded byte string. Decode with `Util.Hex.toBytes`. */
export type Hex = z.infer<typeof hexSchema>;
