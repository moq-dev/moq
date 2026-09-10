/**
 * The moq-transport error code registries, per negotiated draft.
 *
 * A code only means something once you know which draft carried it: the registries grew,
 * moved values, and in draft-15 collapsed five per-message tables into one. Mirrors the
 * Rust `moq_net::ietf::error` module, and the two have to agree value for value.
 *
 * @module
 */

import { type IetfVersion, Version } from "./version.ts";

/**
 * Whether `code` means the same thing in the negotiated draft's stream reset registry as it
 * does in moq-lite's.
 *
 * The two registries agree on most of what they both assign, but this one grew across the
 * drafts we negotiate and moved a value on the way:
 *
 * | Code | draft-14/15 | draft-16/17 | draft-18+ |
 * |------|-------------|-------------|-----------|
 * | 0x0  | INTERNAL_ERROR | INTERNAL_ERROR | INTERNAL_ERROR |
 * | 0x1  | CANCELLED | CANCELLED | CANCELLED |
 * | 0x2  | DELIVERY_TIMEOUT | DELIVERY_TIMEOUT | DELIVERY_TIMEOUT |
 * | 0x3  | SESSION_CLOSED | SESSION_CLOSED | SESSION_CLOSED |
 * | 0x4  | - | UNKNOWN_OBJECT_STATUS | GOING_AWAY |
 * | 0x5  | - | TOO_FAR_BEHIND (17) | TOO_FAR_BEHIND |
 * | 0x12 | - | MALFORMED_TRACK | MALFORMED_TRACK |
 *
 * So GOING_AWAY sent to a draft-17 peer reads as UNKNOWN_OBJECT_STATUS, and that peer's
 * UNKNOWN_OBJECT_STATUS read as GOING_AWAY would retire a session that is not going
 * anywhere. Anything this says `false` about is sent as, and read as, INTERNAL_ERROR: the
 * codes moq-lite keeps in 32-63 (reserved placeholders and its own 48-63 assignments),
 * and the ones the draft assigns that moq-lite has no name for (UNKNOWN_OBJECT_STATUS,
 * EXPIRED_AUTH_TOKEN, EXCESSIVE_LOAD).
 *
 * @internal
 */
export function sharedStreamCode(code: number, version: IetfVersion): boolean {
	switch (code) {
		case 0x0:
		case 0x1:
		case 0x2:
		case 0x3:
			return true;
		// Draft-16 and 17 give 0x4 to UNKNOWN_OBJECT_STATUS, which draft-18 moved to 0x6 when
		// it took 0x4 for GOING_AWAY. Draft-14 and 15 assign it nothing.
		case 0x4:
			return version >= Version.DRAFT_18;
		// TOO_FAR_BEHIND arrived in draft-17.
		case 0x5:
			return version >= Version.DRAFT_17;
		// MALFORMED_TRACK arrived in draft-16.
		case 0x12:
			return version >= Version.DRAFT_16;
		default:
			return false;
	}
}

/**
 * Which request a rejection answers, so draft-14 picks the right registry.
 *
 * Draft-14 gives each error message its own registry and they disagree about 0x4; draft-15
 * folded them all into REQUEST_ERROR with a single registry, so from there on the kind
 * changes nothing.
 *
 * @internal
 */
export type RequestKind = "subscribe" | "fetch" | "publish" | "publish_namespace" | "subscribe_namespace";

/**
 * What a rejection reports, before the draft picks the number for it.
 *
 * Named separately from the wire code because a rejection says less than a failure does: a
 * dozen local errors are one INTERNAL_ERROR on the wire.
 *
 * @internal
 */
export type RequestCondition =
	/** Something went wrong that the registry has no value for. */
	| "internal"
	/** The peer's credentials do not cover the request. */
	| "unauthorized"
	/** The request outlived an implementation-specific deadline. */
	| "timeout"
	/** This endpoint does not implement the request at all. */
	| "not_supported"
	/** The broadcast or track the peer asked for is not here. */
	| "does_not_exist"
	/** The content the peer offered is not wanted here, so it should stop offering it. */
	| "uninterested"
	/** The track's content could not be parsed. */
	| "malformed_track"
	/** A GOAWAY is draining the session, so no new request is accepted. */
	| "going_away";

/** An implementation-specific error. Assigned by every draft, for every request. */
const INTERNAL_ERROR = 0x0;
const UNAUTHORIZED = 0x1;
const TIMEOUT = 0x2;
const NOT_SUPPORTED = 0x3;

/** A draining session. Draft-17 and later. */
const GOING_AWAY = 0x6;

/** Draft-14 calls it TRACK_DOES_NOT_EXIST; draft-15 renamed it and moved it off 0x4. */
const DOES_NOT_EXIST_14 = 0x4;
const DOES_NOT_EXIST = 0x10;

/** Draft-14 assigns UNINTERESTED only to the two requests that offer content. */
const UNINTERESTED_14 = 0x4;
const UNINTERESTED = 0x20;

/** Draft-14 assigns MALFORMED_TRACK to FETCH_ERROR only. */
const MALFORMED_TRACK_14 = 0x9;
const MALFORMED_TRACK = 0x12;

/** The value for "the thing you asked for is not here", or undefined where none is assigned. */
function doesNotExist(kind: RequestKind, version: IetfVersion): number | undefined {
	if (version !== Version.DRAFT_14) return DOES_NOT_EXIST;
	return kind === "subscribe" || kind === "fetch" ? DOES_NOT_EXIST_14 : undefined;
}

/** The value for "we do not want this", or undefined where none is assigned. */
function uninterested(kind: RequestKind, version: IetfVersion): number | undefined {
	if (version !== Version.DRAFT_14) return UNINTERESTED;
	return kind === "publish" || kind === "publish_namespace" ? UNINTERESTED_14 : undefined;
}

/** The value for a track we could not parse, or undefined where none is assigned. */
function malformedTrack(kind: RequestKind, version: IetfVersion): number | undefined {
	if (version !== Version.DRAFT_14) return MALFORMED_TRACK;
	return kind === "fetch" ? MALFORMED_TRACK_14 : undefined;
}

/** The value for a draining session, or undefined before draft-17 registered one. */
function goingAway(version: IetfVersion): number | undefined {
	return version >= Version.DRAFT_17 ? GOING_AWAY : undefined;
}

/**
 * The code to reject a request with, on the negotiated draft.
 *
 * A condition the draft does not register for this request falls back to INTERNAL_ERROR
 * rather than borrowing a number from another draft, which would say something the peer's
 * registry gives a different meaning. The renumbering is the trap: draft-14 SUBSCRIBE_ERROR
 * gives 0x4 to TRACK_DOES_NOT_EXIST and 0x10 to MALFORMED_AUTH_TOKEN, and draft-15 swaps
 * the two.
 *
 * @internal
 */
export function toRequestCode(condition: RequestCondition, kind: RequestKind, version: IetfVersion): number {
	switch (condition) {
		case "internal":
			return INTERNAL_ERROR;
		case "unauthorized":
			return UNAUTHORIZED;
		case "timeout":
			return TIMEOUT;
		case "not_supported":
			return NOT_SUPPORTED;
		case "does_not_exist":
			return doesNotExist(kind, version) ?? INTERNAL_ERROR;
		// A subscriber that asked for content cannot act on "we do not want it": what it needs
		// to know is that we do not have it, which is the same refusal from its side. Only the
		// requests that offer content say UNINTERESTED.
		case "uninterested":
			return kind === "subscribe" || kind === "fetch"
				? (doesNotExist(kind, version) ?? INTERNAL_ERROR)
				: (uninterested(kind, version) ?? INTERNAL_ERROR);
		case "malformed_track":
			return malformedTrack(kind, version) ?? INTERNAL_ERROR;
		case "going_away":
			return goingAway(version) ?? INTERNAL_ERROR;
	}
}

/**
 * Read a rejection code received on the negotiated draft.
 *
 * A code the draft does not assign for this request, INTERNAL_ERROR included, is
 * `undefined`: an error, but never one given a meaning it did not carry.
 *
 * @internal
 */
export function fromRequestCode(code: number, kind: RequestKind, version: IetfVersion): RequestCondition | undefined {
	switch (code) {
		case UNAUTHORIZED:
			return "unauthorized";
		case TIMEOUT:
			return "timeout";
		case NOT_SUPPORTED:
			return "not_supported";
	}

	if (code === doesNotExist(kind, version)) return "does_not_exist";
	if (code === uninterested(kind, version)) return "uninterested";
	if (code === malformedTrack(kind, version)) return "malformed_track";
	if (code === goingAway(version)) return "going_away";
	return undefined;
}

/**
 * Describe a rejection a peer sent, naming the condition when the draft registers one.
 *
 * @internal
 */
export function requestReason(code: number, reasonPhrase: string, kind: RequestKind, version: IetfVersion): string {
	const condition = fromRequestCode(code, kind, version) ?? "unregistered";
	return `${condition} (code=${code}) ${reasonPhrase}`;
}
