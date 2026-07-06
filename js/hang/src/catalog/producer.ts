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
	 * Defaults to `0`, which disables deltas: every change publishes a full snapshot in its own
	 * group, keeping the catalog byte-compatible with snapshot-only consumers. Note this overrides
	 * the `@moq/json` producer's own default ratio. Set a positive number to enable JSON Merge Patch
	 * deltas.
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
			// Pin deltas off by default (the @moq/json default would enable them); pass a positive
			// ratio to opt in.
			deltaRatio: config.deltaRatio ?? 0,
		});
	}

	/** Publish a catalog, including compatibility fields expected by older consumers. */
	override update(value: T): void {
		super.update(withCompatibility(value));
	}
}

function withCompatibility<T extends Root>(catalog: T): T {
	let next = catalog;

	const video = sectionWithCompatibility(catalog.video);
	if (video !== catalog.video) {
		next = { ...next, video } as T;
	}

	const audio = sectionWithCompatibility(catalog.audio);
	if (audio !== catalog.audio) {
		next = { ...next, audio } as T;
	}

	return next;
}

function sectionWithCompatibility(section: unknown): unknown {
	if (!section || typeof section !== "object" || Array.isArray(section)) return section;

	const renditions = (section as { renditions?: unknown }).renditions;
	if (!renditions || typeof renditions !== "object" || Array.isArray(renditions)) return section;

	let nextRenditions = renditions as Record<string, unknown>;

	for (const [name, config] of Object.entries(nextRenditions)) {
		if (!config || typeof config !== "object" || Array.isArray(config)) continue;

		const latencyMin = (config as { latencyMin?: unknown }).latencyMin;
		if (latencyMin === undefined || (config as { jitter?: unknown }).jitter === latencyMin) continue;

		if (nextRenditions === renditions) {
			nextRenditions = { ...nextRenditions };
		}
		nextRenditions[name] = { ...config, jitter: latencyMin };
	}

	if (nextRenditions === renditions) return section;

	return { ...section, renditions: nextRenditions };
}
