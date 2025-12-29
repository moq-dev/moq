// Helper containers for Zod-validated track encoding/decoding.

import type * as z from "zod";
import { Frame } from "./frame.ts";
import type { Group } from "./group.ts";
import type { Track } from "./track.ts";

export async function read<T = unknown>(source: Track | Group, schema: z.ZodSchema<T>): Promise<T | undefined> {
	const next = await source.readFrame();
	if (next === undefined) return; // only treat undefined as EOF, not other falsy values
	return schema.parse(next.toJson());
}

export function write<T = unknown>(source: Track | Group, value: T, schema: z.ZodSchema<T>) {
	const valid = schema.parse(value);
	source.writeFrame(Frame.fromJson(valid));
}
