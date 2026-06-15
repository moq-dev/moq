import type * as Catalog from "@moq/hang/catalog";

import { JsonProducer } from "./json";

/**
 * A stable catalog producer that fans out to on-demand subscription tracks.
 *
 * Unlike a raw track producer, this exists independently of any subscription: edit it at any time
 * with `mutate`, and each subscriber (including a relay that reconnects) is seeded with the current
 * catalog before receiving updates. Independent owners (the base `video`/`audio` and an
 * application's own sections, e.g. `scte35`) each edit only their own keys, so their sections
 * compose instead of clobbering one another.
 */
export class CatalogProducer extends JsonProducer<Catalog.Root> {
	/** Create a catalog producer seeded with an empty catalog. */
	constructor() {
		super({ initial: {} });
	}
}
