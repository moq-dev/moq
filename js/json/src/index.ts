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
 * Each mode comes in two layers. `Producer`/`Consumer` own a track and manage its groups.
 * `Encoder`/`Decoder` are the same logic without the track: values in, frame payloads out (and
 * back), with the encoder saying where the group boundaries fall. Reach for the codec layer when
 * something else already owns the track.
 *
 * @module
 */

export { type Diff, deepEqual, diff, merge } from "./diff.ts";
export * as Snapshot from "./snapshot/index.ts";
export * as Stream from "./stream/index.ts";
