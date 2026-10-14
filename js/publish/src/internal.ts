import type * as Moq from "@moq/net";
import type { Getter } from "@moq/signals";

/** @internal Terminal failures of broadcast-owned rendition tracks, including idle tracks. */
export const errors = new WeakMap<object, Getter<Error | undefined>>();

/** @internal Pair an encoder config with the network epoch whose bounds it describes. */
export type CatalogState = { epoch: Moq.Broadcast.Producer | undefined; config: unknown };
/** @internal Encoder catalog states keyed by their registered rendition. */
export const catalogs = new WeakMap<object, Getter<CatalogState | undefined>>();
