import type * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import type { Effect } from "@moq/signals";

/**
 * A stable catalog producer that fans out to on-demand subscription tracks.
 *
 * Unlike a raw track producer, this exists independently of any subscription: edit it at any time
 * with {@link mutate}, and each subscriber (including a relay that reconnects) is seeded with the
 * current catalog before receiving updates. Independent owners (the base `video`/`audio` and an
 * application's own sections, e.g. `scte35`) each edit only their own keys, so their sections
 * compose instead of clobbering one another.
 */
export class CatalogProducer {
	#value: Catalog.Root = {};
	#outputs = new Set<Json.Producer<Catalog.Root>>();

	/** Edit the catalog in place; the result is published to all current subscribers. */
	mutate(fn: (catalog: Catalog.Root) => void): void {
		const value = structuredClone(this.#value);
		fn(value);
		this.#value = value;
		for (const output of this.#outputs) output.update(value);
	}

	/**
	 * Serve a subscription request: seed it with the current catalog, then forward updates.
	 *
	 * Pass `opts.compression` to DEFLATE-compress this subscriber's frames, so the same catalog can be
	 * served both plaintext and compressed (e.g. `catalog.json` and `catalog.json.z`).
	 */
	serve(track: Moq.TrackProducer, effect: Effect, opts?: { compression?: boolean }): void {
		const output = new Json.Producer<Catalog.Root>(track, { compression: opts?.compression });
		output.update(this.#value);

		this.#outputs.add(output);
		effect.cleanup(() => {
			this.#outputs.delete(output);
			output.finish();
		});
	}
}
