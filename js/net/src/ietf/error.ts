/**
 * The moq-transport error code registries, per negotiated draft.
 *
 * Draft-22 added INVALID_FILTER and removed INVALID_JOINING_REQUEST_ID; the rest
 * holds from draft-18 on. Mirrors the Rust `moq_net::ietf::error` module, and the two have
 * to agree value for value.
 *
 * @module
 */

import { type IetfVersion, Version } from "./version.ts";

/**
 * Whether `code` means the same thing in the moq-transport stream reset registry as it does
 * in moq-lite's.
 *
 * The two agree on 0x0-0x5 (INTERNAL_ERROR, CANCELLED, DELIVERY_TIMEOUT, SESSION_CLOSED,
 * GOING_AWAY, TOO_FAR_BEHIND) and 0x12 (MALFORMED_TRACK). Anything else is sent as, and read
 * as, INTERNAL_ERROR: the codes moq-lite assigns in its own 48-63 range, and the ones the
 * draft assigns that moq-lite has no name for (UNKNOWN_OBJECT_STATUS, EXPIRED_AUTH_TOKEN,
 * EXCESSIVE_LOAD).
 *
 * @internal
 */
export function sharedStreamCode(code: number): boolean {
	return (code >= 0x0 && code <= 0x5) || code === 0x12;
}

/**
 * Which request a rejection answers: some codes are registered for only some requests.
 *
 * @internal
 */
export type RequestKind =
	| "subscribe"
	| "fetch"
	| "publish"
	| "publish_namespace"
	| "subscribe_namespace"
	| "track_status";

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
	/** The token expired and can be refreshed. */
	| "expired"
	/** The request outlived an implementation-specific deadline. */
	| "timeout"
	/** This endpoint does not implement the request at all. */
	| "not_supported"
	/** The broadcast or track the peer asked for is not here. */
	| "invalid_range"
	| "invalid_filter"
	| "invalid_joining_request_id"
	| "does_not_exist"
	/** The content the peer offered is not wanted here, so it should stop offering it. */
	| "uninterested"
	/** The track's content could not be parsed. */
	| "malformed_track"
	/** A well-formed AUTHORIZATION TOKEN this endpoint cannot use, such as a repeat. */
	| "malformed_auth_token"
	/** A GOAWAY is draining the session, so no new request is accepted. */
	| "going_away";

/** An implementation-specific error. Assigned by every draft, for every request. */
const INTERNAL_ERROR = 0x0;
const UNAUTHORIZED = 0x1;
const TIMEOUT = 0x2;
const NOT_SUPPORTED = 0x3;

const GOING_AWAY = 0x6;
const MALFORMED_AUTH_TOKEN = 0x4;
const EXPIRED_AUTH_TOKEN = 0x5;
const DOES_NOT_EXIST = 0x10;
const MALFORMED_TRACK = 0x12;
const UNINTERESTED = 0x20;

/** FETCH-only errors. Draft-22 has no INVALID_JOINING_REQUEST_ID. */
const INVALID_RANGE = 0x11;
const INVALID_JOINING_REQUEST_ID = 0x32;

/** Draft-22 added it. */
const INVALID_FILTER = 0x36;

function invalidRange(kind: RequestKind): number | undefined {
	return kind === "fetch" ? INVALID_RANGE : undefined;
}

function invalidJoiningRequestId(kind: RequestKind, version: IetfVersion): number | undefined {
	return kind === "fetch" && version <= Version.DRAFT_18 ? INVALID_JOINING_REQUEST_ID : undefined;
}

/**
 * The code to reject a request with, on the negotiated draft.
 *
 * A condition the draft does not register for this request falls back to INTERNAL_ERROR
 * rather than borrowing a number from another draft, which would say something the peer's
 * registry gives a different meaning.
 *
 * @internal
 */
export function toRequestCode(condition: RequestCondition, kind: RequestKind, version: IetfVersion): number {
	switch (condition) {
		case "invalid_filter":
			return version >= Version.DRAFT_20 ? INVALID_FILTER : INTERNAL_ERROR;
		case "internal":
			return INTERNAL_ERROR;
		case "unauthorized":
			return UNAUTHORIZED;
		case "expired":
			return EXPIRED_AUTH_TOKEN;
		case "timeout":
			return TIMEOUT;
		case "not_supported":
			return NOT_SUPPORTED;
		case "invalid_range":
			return invalidRange(kind) ?? INTERNAL_ERROR;
		case "invalid_joining_request_id":
			return invalidJoiningRequestId(kind, version) ?? INTERNAL_ERROR;
		case "does_not_exist":
			return DOES_NOT_EXIST;
		// A subscriber that asked for content cannot act on "we do not want it": what it needs
		// to know is that we do not have it, which is the same refusal from its side. Only the
		// requests that offer content say UNINTERESTED.
		case "uninterested":
			return kind === "subscribe" || kind === "fetch" || kind === "track_status" ? DOES_NOT_EXIST : UNINTERESTED;
		case "malformed_track":
			return MALFORMED_TRACK;
		case "malformed_auth_token":
			return MALFORMED_AUTH_TOKEN;
		case "going_away":
			return GOING_AWAY;
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

	if (code === INVALID_FILTER && version >= Version.DRAFT_20) return "invalid_filter";
	if (code === DOES_NOT_EXIST) return "does_not_exist";
	if (code === invalidRange(kind)) return "invalid_range";
	if (code === invalidJoiningRequestId(kind, version)) return "invalid_joining_request_id";
	if (code === EXPIRED_AUTH_TOKEN) return "expired";
	if (code === UNINTERESTED) return "uninterested";
	if (code === MALFORMED_TRACK) return "malformed_track";
	if (code === GOING_AWAY) return "going_away";
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
