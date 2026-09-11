/**
 * MoQ networking layer for browsers: connect to a relay, then publish and consume
 * broadcasts, tracks, groups, and frames over WebTransport (or a WebSocket fallback).
 *
 * @module
 */

/** Re-export of {@link https://jsr.io/@moq/signals | @moq/signals}, the reactive primitives used throughout this package. */
export * as Signals from "@moq/signals";
/** Broadcast announcement streams. */
export * as Announce from "./announced.ts";
/** Send-side bandwidth estimates split among the tracks sharing a connection. */
export * as Bandwidth from "./bandwidth.ts";
/** Broadcast role handles. */
export * as Broadcast from "./broadcast.ts";
/** A reconnecting, shareable handle on a MoQ session. */
export { Connection } from "./connection/index.ts";
/** Session and stream errors, each carrying a code from its own registry. */
export {
	NotFound,
	SessionCode,
	SessionError,
	StreamCode,
	StreamError,
	type StreamErrorOptions,
} from "./error.ts";
/** Group role handles and frame helpers. */
export * as Group from "./group.ts";
/** Broadcast routing tables, independent of any connection. */
export * as Origin from "./origin.ts";
/** Broadcast path utilities with delimiter-aware prefix matching. */
export * as Path from "./path.ts";
/** Branded time types (nanoseconds, microseconds, milliseconds, seconds) with conversions. */
export * as Time from "./time.ts";
/** Track role handles. */
export * as Track from "./track.ts";
/** QUIC variable-length integer encoding and decoding. */
export * as Varint from "./varint.ts";
