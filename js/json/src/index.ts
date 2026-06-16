/**
 * Snapshot/delta JSON publishing over MoQ tracks using RFC 7396 JSON Merge Patch. A
 * {@link Producer} writes a JSON value to one track or fans it out to many; a {@link Consumer}
 * reconstructs the value on the other side.
 *
 * @module
 */

export { Consumer } from "./consumer.ts";
export { type Diff, deepEqual, diff, merge } from "./diff.ts";
export { type Config, Producer } from "./producer.ts";
