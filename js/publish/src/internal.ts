import type { Getter } from "@moq/signals";

/** @internal Terminal failures of broadcast-owned rendition tracks, including idle tracks. */
export const errors = new WeakMap<object, Getter<Error | undefined>>();
