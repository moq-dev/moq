/**
 * JSON publishing over MoQ tracks, in three modes:
 *
 * - {@link Snapshot}: **lossy**. One JSON value updated over time; a consumer only gets the most
 *   recent value. Intermediate updates are collapsed and older groups are dropped.
 * - {@link Stream}: **lossless**. An ordered append-log of self-contained records; every record
 *   is preserved and delivered in order, nothing is ever superseded.
 * - {@link Window}: **sliding**. An ordered log that appends at the tail and removes from the
 *   head, like an HLS playlist. Records carry stable positions and old groups are disposable.
 *
 * Pick {@link Snapshot} when consumers care about "what is the value now" (a catalog, a status
 * document), {@link Stream} when they care about every record ever written (an event log), and
 * {@link Window} when records expire (a media timeline over a bounded cache).
 *
 * {@link Snapshot} and {@link Stream} each come in two layers. `Producer`/`Consumer` own a track
 * and manage its groups. `Encoder`/`Decoder` are the same logic without the track: values in,
 * frame payloads out (and back), with the encoder saying where the group boundaries fall. Reach
 * for the codec layer when something else already owns the track.
 *
 * {@link Window} is the one layer: where its groups roll follows from its own trim history, so it
 * owns the track outright.
 *
 * @module
 */

export { type Diff, deepEqual, diff, merge } from "./diff.ts";
export * as Snapshot from "./snapshot/index.ts";
export * as Stream from "./stream/index.ts";
export * as Window from "./window.ts";
