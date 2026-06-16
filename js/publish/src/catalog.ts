import type * as Catalog from "@moq/hang/catalog";
import type { Producer } from "@moq/json";

/**
 * The broadcast catalog: a track-less {@link Producer} of the catalog {@link Catalog.Root}, seeded
 * empty.
 *
 * Edit it at any time with `mutate` (the base `video`/`audio` sections are kept in sync by the
 * encoders; an application adds its own root sections, e.g. `scte35`, the same way). Each
 * subscriber, including a relay that reconnects, is seeded with the current catalog before updates.
 */
export type CatalogProducer = Producer<Catalog.Root>;
