/**
 * JSON publishing over MoQ tracks, in two modes:
 *
 * - {@link Snapshot}: **lossy**. One JSON value updated over time; a consumer only gets the most
 *   recent value. Intermediate updates are collapsed and older groups are dropped.
 * - {@link Stream}: **lossless**. An ordered append-log of self-contained records; every record
 *   is preserved and delivered in order, nothing is ever superseded.
 *
 * Pick {@link Snapshot} when consumers care about "what is the value now" (a catalog, a status
 * document) and {@link Stream} when they care about every record (an event log, a media timeline).
 *
 * @module
 */

export { type Diff, deepEqual, diff, merge } from "./diff.ts";
export * as Snapshot from "./snapshot/index.ts";
export * as Stream from "./stream.ts";
