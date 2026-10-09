/**
 * Lossless append-log opaque publishing over MoQ tracks.
 *
 * An ordered log of opaque payloads, for consumers that care about every one (an event log, a
 * sequence of samples). Nothing is ever superseded: a consumer yields each payload in the order it
 * was appended. For a latest-value document, use the `Snapshot` module instead.
 *
 * On the wire the log rides a **single group** that is never rolled, one payload per frame. A
 * payload that cannot be written ends the track rather than opening a second group: a log missing a
 * record is not lossless, and a gap dressed up as a complete log is worse than a visible failure.
 * With {@link Config.compression} `"deflate"`, that one group is one DEFLATE window, so each payload
 * compresses against the earlier ones and a run of similar payloads shrinks sharply.
 *
 * That one group bounds the log at `@moq/net`'s group budget: 32 MiB of payload and 8192 frames. A
 * consumer always starts at frame 0, so the cap is never met by dropping a prefix some readers
 * missed. Instead an append that might not fit throws `GroupTooLarge` before it is encoded, and the
 * log stays intact and writable. With compression the check counts the payload's raw size plus
 * DEFLATE's worst-case overhead (`Encoder.bound`), since the compressed size is only known once the
 * window has moved. The budget covers the whole log, so once it is spent every append throws and a
 * publisher with more to say opens a new track.
 *
 * @module
 */

export { Consumer, Rolled } from "./consumer.ts";
export { type Config, Producer } from "./producer.ts";
