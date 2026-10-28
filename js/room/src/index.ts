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

/** Chat messages exchanged in the room. */
export * as Chat from "./chat.ts";
export { Local, type LocalProps } from "./local.ts";
export {
	type Consumed,
	consume,
	type ExtendedCatalog,
	type HangCatalog,
	PRIORITY,
	type Preview,
	serve,
	TRACK,
	type TrackRef,
	type User,
	type UserFields,
	type UserInput,
	type UserProps,
	userFields,
} from "./metadata.ts";
export { broadcastPath, isKind, KIND, type Kind, kindFromSegment, type Parsed, parse } from "./path.ts";
export { Member, Remote, type RemoteProps } from "./remote.ts";
export { Room, type RoomProps } from "./room.ts";
export { type Claims, claims } from "./token.ts";
