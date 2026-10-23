/**
 * Headless multi-participant rooms over MoQ.
 *
 * A room is a path prefix. The connection URL and token root already carry it;
 * this package has no service and no storage. Participants are discovered from
 * the announce stream, identity is the path before `camera`/`screen`, and each
 * participant publishes `{identity}/camera.hang` (camera + mic, hd/sd) and
 * `{identity}/screen.hang` (screenshare, whose announce/unannounce is the share
 * lifecycle).
 *
 * @module
 */

/** Re-export of {@link https://jsr.io/@moq/clock | @moq/clock}, the time units this package's signatures take. */
export * as Clock from "@moq/clock";
/** Re-export of {@link https://jsr.io/@moq/hang | @moq/hang}, the media layer. */
export * as Hang from "@moq/hang";
/** Re-export of {@link https://jsr.io/@moq/net | @moq/net}, the transport. */
export * as Net from "@moq/net";
/** Re-export of `@moq/publish`, which publishes each participant's media. */
export * as Publish from "@moq/publish";
/** Re-export of {@link https://jsr.io/@moq/signals | @moq/signals}, the reactive primitives used throughout this package. */
export * as Signals from "@moq/signals";
/** Re-export of `@moq/watch`, which plays each participant's media. */
export * as Watch from "@moq/watch";

/** Chat messages exchanged in the room. */
export * as Chat from "./chat.ts";
export { Local, type LocalProps } from "./local.ts";
export {
	consume,
	type ExtendedCatalog,
	type HangCatalog,
	PRIORITY,
	type Preview,
	serve,
	TRACK,
	type User,
	type UserFields,
	type UserProps,
	userFields,
} from "./metadata.ts";
export { broadcastPath, isKind, KIND, type Kind, kindFromSegment, type Parsed, parse } from "./path.ts";
export { Member, Remote, type RemoteProps } from "./remote.ts";
export { Room, type RoomProps } from "./room.ts";
export { type Claims, claims } from "./token.ts";
