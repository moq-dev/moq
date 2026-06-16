/**
 * Helpers for reading and writing Zod-validated JSON frames on a track or group.
 *
 * @module
 */

import type * as z from "zod/mini";
import type { Group } from "./group.ts";
import type { Track } from "./track.ts";

/** Read the next JSON frame and validate it against the schema. Returns undefined at end of stream. */
export async function read<T = unknown>(source: Track | Group, schema: z.ZodMiniType<T>): Promise<T | undefined> {
	const next = await source.readJson();
	if (next === undefined) return undefined; // only treat undefined as EOF, not other falsy values
	return schema.parse(next);
}

/** Validate a value against the schema, then write it as a JSON frame. */
export function write<T = unknown>(source: Track | Group, value: T, schema: z.ZodMiniType<T>) {
	const valid = schema.parse(value);
	source.writeJson(valid);
}
