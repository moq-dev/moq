/**
 * MoQ networking layer for browsers: connect to a relay, then publish and consume
 * broadcasts, tracks, groups, and frames over WebTransport, its WebSocket fallback, or a supplied transport.
 *
 * @module
 */

/** Re-export of {@link https://www.npmjs.com/package/@moq/clock | @moq/clock}, the durations, instants, and timers this package takes. */
export * as Clock from "@moq/clock";
/** Re-export of {@link https://www.npmjs.com/package/@moq/signals | @moq/signals}, the reactive primitives used throughout this package. */
export * as Signals from "@moq/signals";
/** Broadcast announcement streams. */
export * as Announce from "./announce.ts";
/** In-band authorization: tokens presented to the peer and the grants they earn. */
export * as Auth from "./auth.ts";
/** Send-side bandwidth estimates split among the tracks sharing a connection. */
export * as Bandwidth from "./bandwidth_api.ts";
/** Broadcast role handles. */
export * as Broadcast from "./broadcast.ts";
/** A reconnecting, shareable handle on a MoQ session. */
export { Connection } from "./connection/index.ts";
/** Live datagram track producers, consumers, and payloads. */
export * as Datagram from "./datagram.ts";
/** The codes a session closes with and a stream resets with. */
export { SessionCode, StreamCode } from "./error.ts";
/** Session, stream, and local request errors. */
export * as Error from "./errors.ts";
/** Frame role handles: one frame's payload written or read in pieces. */
export * as Frame from "./frame.ts";
/** Group role handles. */
export * as Group from "./group.ts";
/** Stream identities: one continuous stream at a path, carried on every route. */
export * as Identity from "./identity.ts";
/** Metadata subscriptions: a track's groups and frame headers without the payload. */
export * as Meta from "./meta.ts";
/** Broadcast routing tables, independent of any connection. */
export * as Origin from "./origin.ts";
/** Broadcast path utilities with delimiter-aware prefix matching, plus grant prefixes. */
export * as Path from "./path.ts";
/** Presentation timestamps and their timescales; durations live in {@link Clock}. */
export * as Time from "./time.ts";
/** A value paired with its frame's timestamp: when it was captured, or a scale-less track's group key. */
export type { Timed } from "./timed.ts";
/** Track role handles. */
export * as Track from "./track.ts";
/** QUIC varint encoding and decoding (RFC 9000 Section 16). */
export * as Varint from "./varint.ts";
