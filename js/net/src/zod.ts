/**
 * Helpers for reading and writing Zod-validated JSON frames on a track or group.
 *
 * @module
 */

import type * as z from "zod/mini";
import type * as group from "./group.ts";
import type * as track from "./track.ts";

/** Read the next JSON frame and validate it against the schema. Returns undefined at end of stream. */
export async function read<T = unknown>(
	source: track.Subscriber | group.Consumer,
	schema: z.ZodMiniType<T>,
): Promise<T | undefined> {
	const next = await source.readJson();
	if (next === undefined) return undefined; // only treat undefined as EOF, not other falsy values
	return schema.parse(next);
}

/** Validate a value against the schema, then write it as a JSON frame. */
export function write<T = unknown>(source: track.Producer | group.Producer, value: T, schema: z.ZodMiniType<T>) {
	const valid = schema.parse(value);
	source.writeJson(valid);
}
