import * as Json from "@moq/json";
import type * as z from "zod/mini";

import { type Root, RootSchema } from "./root.ts";

/** Options for a catalog {@link Producer}. */
export interface Config<T extends Root = Root> {
	/** zod schema validating each catalog before publish. Defaults to {@link RootSchema}. */
	schema?: z.ZodMiniType<T>;

	/**
	 * Delta encoding ratio forwarded to the underlying JSON producer.
	 *
	 * Defaults to `0`, which (like `undefined`) disables deltas: every change publishes a full
	 * snapshot in its own group, matching the Rust catalog producer (`delta_ratio: None`) and the
	 * current wire. Set a positive number to enable JSON Merge Patch deltas.
	 */
	deltaRatio?: number;
}

/**
 * Publishes a {@link Root} catalog, fanning it out to every subscriber (including relays that
 * reconnect).
 *
 * A thin wrapper around a track-less `@moq/json` producer, pre-seeded with an empty catalog and
 * wired to {@link RootSchema}. Edit it at any time with `mutate` and answer subscription requests
 * with `serve`. Extend the catalog by passing a schema built via `z.extend(RootSchema, ...)` and
 * writing the extra sections in `mutate`.
 */
export class Producer<T extends Root = Root> extends Json.Producer<T> {
	/** Create a track-less catalog producer seeded with an empty catalog. */
	constructor(config: Config<T> = {}) {
		super({
			initial: {} as T,
			schema: (config.schema ?? RootSchema) as z.ZodMiniType<T>,
			// Deltas off by default (one snapshot per group); pass a positive ratio to enable.
			deltaRatio: config.deltaRatio ?? 0,
		});
	}
}
