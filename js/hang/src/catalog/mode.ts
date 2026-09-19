import * as z from "zod/mini";

/** The modes this build knows how to read. */
export type KnownMode = "snapshot" | "stream";

/**
 * How a data track's groups carry its payloads.
 *
 * - `"snapshot"`: lossy. Each group is self-contained and supersedes the previous one, so a
 *   consumer reads only the newest. A JSON track may follow a group's first frame with RFC 7396
 *   merge-patch deltas; a binary track writes one frame per group.
 * - `"stream"`: lossless. A single group, never rolled, one payload per frame in order.
 *
 * Always stated by the publisher: there is no default, because reading an append log as a
 * latest-value document silently discards every payload but the last.
 *
 * Typed as a plain string so an unrecognized mode survives a reparse-and-republish verbatim.
 * Narrow it with {@link modeSupported} before reading the track, and ignore the track otherwise.
 */
export const ModeSchema = z.string();

/** How a data track's groups carry its payloads. See {@link ModeSchema}. */
export type Mode = z.infer<typeof ModeSchema>;

/** Whether this build knows how to read a track published in `mode`. */
export function modeSupported(mode: Mode): mode is KnownMode {
	return mode === "snapshot" || mode === "stream";
}
