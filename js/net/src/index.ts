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
/** Broadcast role handles. */
export * as Broadcast from "./broadcast.ts";
/** Connection helpers: connect to or accept a MoQ session and reconnect on failure. */
export * as Connection from "./connection/index.ts";
/** The error a read or write rejects with when the peer resets a stream, carrying its code. */
export { RemoteError } from "./error.ts";
/** Group role handles and frame helpers. */
export * as Group from "./group.ts";
/** Broadcast path utilities with delimiter-aware prefix matching. */
export * as Path from "./path.ts";
/** Retry policy: which failures are worth repeating, and how long to wait before repeating them. */
export * as Retry from "./retry.ts";
/** Branded time types (nanoseconds, microseconds, milliseconds, seconds) with conversions. */
export * as Time from "./time.ts";
/** Track role handles. */
export * as Track from "./track.ts";
/** QUIC variable-length integer encoding and decoding. */
export * as Varint from "./varint.ts";
