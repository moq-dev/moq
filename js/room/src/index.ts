/**
 * Headless multi-participant rooms over MoQ.
 *
 * A room is a path prefix. The connection URL and token root already carry it;
 * this package has no service and no storage. Participants are discovered from
 * the announce stream, identity is the path before `camera`/`screen`, and each
 * participant publishes `{identity}/camera` (camera + mic, hd/sd) and
 * `{identity}/screen` (screenshare, whose announce/unannounce is the share
 * lifecycle).
 *
 * @module
 */

export * as Hang from "@moq/hang";
export * as Net from "@moq/net";
export * as Publish from "@moq/publish";
export * as Signals from "@moq/signals";
export * as Watch from "@moq/watch";

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
