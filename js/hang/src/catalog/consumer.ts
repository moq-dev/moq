import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import type * as z from "zod/mini";

import { type Root, RootSchema } from "./root.ts";

/**
 * Consumes a {@link Root} catalog from a track, reconstructing it from snapshots and deltas.
 *
 * A thin wrapper around the `@moq/json` consumer, pre-wired with {@link RootSchema}. Call `next()`
 * to get each catalog as it changes, or iterate it. Pass an extended schema (built via
 * `z.extend(RootSchema, ...)`) to validate and type application sections; otherwise unknown
 * sections pass through untouched.
 */
export class Consumer<T extends Root = Root> extends Json.Consumer<T> {
	/** Wrap `track`, validating each catalog against `schema` (defaults to {@link RootSchema}). */
	constructor(track: Moq.Track, schema?: z.ZodMiniType<T>) {
		super(track, { schema: (schema ?? RootSchema) as z.ZodMiniType<T> });
	}
}

/**
 * Read the current catalog from `track` once.
 *
 * @deprecated Use {@link Consumer} instead: `new Catalog.Consumer(track).next()`. A one-shot read
 * returns only the current catalog and misses later updates (and deltas, once enabled).
 */
export function fetch(track: Moq.Track): Promise<Root | undefined> {
	return new Consumer(track).next();
}
