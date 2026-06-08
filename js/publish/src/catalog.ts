import type * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import type { Effect } from "@moq/signals";

/**
 * A stable catalog producer that fans out to on-demand subscription tracks.
 *
 * Unlike a raw track producer, this exists independently of any subscription: edit it at any time
 * with {@link lock}, and each subscriber (including a relay that reconnects) is seeded with the
 * current catalog before receiving updates. Independent owners (the base `video`/`audio` and an
 * application's own sections, e.g. `scte35`) each lock and edit only their own keys, so their
 * sections compose instead of clobbering one another.
 */
export class CatalogProducer {
	#value: Catalog.Root = {};
	#outputs = new Set<Json.Producer<Catalog.Root>>();

	/** Edit the catalog in place; the result is published to all current subscribers on dispose. */
	lock(): Json.Guard<Catalog.Root> {
		const value = structuredClone(this.#value);
		return {
			value,
			[Symbol.dispose]: () => {
				this.#value = value;
				for (const output of this.#outputs) output.update(value);
			},
		};
	}

	/** Serve a subscription request: seed it with the current catalog, then forward updates. */
	serve(track: Moq.Track, effect: Effect): void {
		const output = new Json.Producer<Catalog.Root>(track);
		output.update(this.#value);

		this.#outputs.add(output);
		effect.cleanup(() => {
			this.#outputs.delete(output);
			output.finish();
		});
	}
}
