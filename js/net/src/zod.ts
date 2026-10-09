/**
 * Helpers for reading Zod-validated JSON frames from a track or group.
 *
 * @module
 */

import type * as z from "@zod/mini";
import type * as group from "./group.ts";
import type * as track from "./track.ts";

/** Read the next JSON frame and validate it against the schema. Returns undefined at end of stream. */
export async function read<T = unknown>(
	source: track.Ordered | group.Consumer,
	schema: z.ZodMiniType<T>,
): Promise<T | undefined> {
	const next = await source.readJson();
	if (next === undefined) return undefined; // only treat undefined as EOF, not other falsy values
	return schema.parse(next);
}
