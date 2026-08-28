import * as z from "zod/mini";

/** The compression algorithms this build knows how to decode. */
export type KnownCompression = "deflate";

/**
 * The compression applied to a data track's frames, or absent when they are uncompressed.
 *
 * `"deflate"` is group-scoped raw DEFLATE (RFC 1951), sync-flushed at each frame boundary: every
 * frame is self-delimited while later frames compress against the earlier ones in the same group.
 *
 * A track's catalog entry declares this explicitly. The `.z` suffix seen on some track names is a
 * naming convention, not a signal: never infer compression from a track's name.
 *
 * Typed as a plain string so an unrecognized algorithm survives a reparse-and-republish verbatim.
 * Narrow it with {@link compressionSupported} before reading the track, and ignore the track
 * otherwise, since its frames cannot be decoded.
 */
export const CompressionSchema = z.string();

/** The compression applied to a data track's frames. See {@link CompressionSchema}. */
export type Compression = z.infer<typeof CompressionSchema>;

/** Whether this build knows how to decode frames compressed with `compression`. */
export function compressionSupported(compression: Compression): compression is KnownCompression {
	return compression === "deflate";
}
