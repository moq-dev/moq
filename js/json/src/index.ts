/**
 * Snapshot/delta JSON publishing over MoQ tracks using RFC 7396 JSON Merge Patch: a per-track
 * {@link Producer}/{@link Consumer} pair and a {@link Source} that fans one value out to many
 * subscribers.
 *
 * @module
 */

export { Consumer } from "./consumer.ts";
export { type Diff, deepEqual, diff, merge } from "./diff.ts";
export { type Config, Producer } from "./producer.ts";
export { Source } from "./source.ts";
