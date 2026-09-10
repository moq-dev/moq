import { expect, test } from "bun:test";
import { StreamCode } from "../error.ts";
import { fromRequestCode, type RequestCondition, type RequestKind, sharedStreamCode, toRequestCode } from "./error.ts";
import { type IetfVersion, Version } from "./version.ts";

const ALL: IetfVersion[] = [
	Version.DRAFT_14,
	Version.DRAFT_15,
	Version.DRAFT_16,
	Version.DRAFT_17,
	Version.DRAFT_18,
	Version.DRAFT_19,
	Version.DRAFT_20,
];

const KINDS: RequestKind[] = ["subscribe", "fetch", "publish", "publish_namespace", "subscribe_namespace"];

/** Every condition a rejection can carry. A new one belongs here. */
const CONDITIONS: RequestCondition[] = [
	"internal",
	"unauthorized",
	"timeout",
	"not_supported",
	"does_not_exist",
	"uninterested",
	"malformed_track",
	"going_away",
];

/**
 * Every code we put in a rejection has to be one the negotiated draft registers for that
 * request. The table is transcribed from the drafts (draft-14 section 13.1 through draft-20
 * section 15.11.2), not derived from the mapping, so a mistake in the mapping cannot talk
 * the assertion into agreeing with it.
 */
function registered(kind: RequestKind, version: IetfVersion): number[] {
	if (version === Version.DRAFT_14) {
		switch (kind) {
			case "subscribe":
				return [0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x10, 0x12];
			case "fetch":
				return [0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x7, 0x8, 0x9, 0x10, 0x12];
			case "publish":
				return [0x0, 0x1, 0x2, 0x3, 0x4];
			case "publish_namespace":
				return [0x0, 0x1, 0x2, 0x3, 0x4, 0x10, 0x12];
			case "subscribe_namespace":
				return [0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x10, 0x12];
		}
	}
	if (version === Version.DRAFT_15) return [0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x10, 0x11, 0x12, 0x20, 0x30, 0x32, 0x33];
	if (version === Version.DRAFT_16) return [0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x10, 0x11, 0x12, 0x19, 0x20, 0x30, 0x32];
	if (version === Version.DRAFT_17) {
		return [0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x9, 0x10, 0x11, 0x12, 0x19, 0x20, 0x30, 0x31, 0x32];
	}
	return [
		0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x9, 0x10, 0x11, 0x12, 0x19, 0x20, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36,
	];
}

test("only registered codes reach the wire", () => {
	for (const version of ALL) {
		for (const kind of KINDS) {
			for (const condition of CONDITIONS) {
				const code = toRequestCode(condition, kind, version);
				expect(registered(kind, version)).toContain(code);
			}
		}
	}
});

test("every emitted code round trips", () => {
	for (const version of ALL) {
		for (const kind of KINDS) {
			for (const condition of CONDITIONS) {
				const code = toRequestCode(condition, kind, version);
				const decoded = fromRequestCode(code, kind, version) ?? "internal";
				expect(toRequestCode(decoded, kind, version)).toBe(code);
			}
		}
	}
});

/**
 * Draft-14 numbers a missing track 0x4 and draft-15 moved it to 0x10, which is draft-14's
 * MALFORMED_AUTH_TOKEN. Getting this backwards tells a peer its token is broken when the
 * broadcast simply is not here, so it re-authenticates instead of waiting.
 */
test("a missing broadcast uses the draft's own number", () => {
	for (const kind of ["subscribe", "fetch"] as const) {
		expect(toRequestCode("does_not_exist", kind, Version.DRAFT_14)).toBe(0x4);
		expect(fromRequestCode(0x4, kind, Version.DRAFT_14)).toBe("does_not_exist");

		for (const version of ALL.slice(1)) {
			expect(toRequestCode("does_not_exist", kind, version)).toBe(0x10);
			expect(fromRequestCode(0x10, kind, version)).toBe("does_not_exist");
			// 0x4 is MALFORMED_AUTH_TOKEN from draft-15 on, which is not ours to claim.
			expect(fromRequestCode(0x4, kind, version)).toBeUndefined();
		}
	}
});

/**
 * Draft-14 gives 0x4 to UNINTERESTED on the requests that offer content and to
 * TRACK_DOES_NOT_EXIST on the ones that ask for it, so the same integer on the same draft
 * says two different things.
 */
test("draft-14 reads 0x4 per request", () => {
	for (const kind of ["publish", "publish_namespace"] as const) {
		expect(toRequestCode("uninterested", kind, Version.DRAFT_14)).toBe(0x4);
		expect(fromRequestCode(0x4, kind, Version.DRAFT_14)).toBe("uninterested");
		// Those requests offer content, so "it is not here" is not a rejection they carry.
		expect(toRequestCode("does_not_exist", kind, Version.DRAFT_14)).toBe(0x0);
	}

	expect(fromRequestCode(0x4, "subscribe_namespace", Version.DRAFT_14)).toBeUndefined();
});

/** GOING_AWAY arrived in draft-17; an earlier peer is told INTERNAL_ERROR instead. */
test("going away only exists from draft-17", () => {
	for (const kind of KINDS) {
		for (const version of [Version.DRAFT_14, Version.DRAFT_15, Version.DRAFT_16] as const) {
			expect(toRequestCode("going_away", kind, version)).toBe(0x0);
			expect(fromRequestCode(0x6, kind, version)).toBeUndefined();
		}

		for (const version of [Version.DRAFT_17, Version.DRAFT_18, Version.DRAFT_19, Version.DRAFT_20] as const) {
			expect(toRequestCode("going_away", kind, version)).toBe(0x6);
			expect(fromRequestCode(0x6, kind, version)).toBe("going_away");
		}
	}
});

/**
 * Draft-18 took 0x4 for GOING_AWAY from UNKNOWN_OBJECT_STATUS, so the same integer means
 * different things on two drafts we both negotiate. Sending it to draft-17 would claim the
 * next object's status is unknowable; reading theirs as GOING_AWAY would start draining a
 * session that is not going anywhere.
 */
test("stream reset codes are shared only where the draft agrees", () => {
	for (const version of ALL) {
		for (const code of [
			StreamCode.Internal,
			StreamCode.Cancel,
			StreamCode.DeliveryTimeout,
			StreamCode.SessionClosed,
		]) {
			expect(sharedStreamCode(code, version)).toBe(true);
		}

		// Lite-only: the assigned 48-63 cache-miss codes and the reserved 32-47
		// placeholders are not in this registry at all, so no unassigned lite value
		// ever reaches a moq-transport peer.
		for (const code of [
			StreamCode.NotFound,
			StreamCode.Old,
			StreamCode.Evicted,
			StreamCode.FrameTooLarge,
			StreamCode.GroupTooLarge,
		]) {
			expect(sharedStreamCode(code, version)).toBe(false);
		}
	}

	for (const version of [Version.DRAFT_14, Version.DRAFT_15, Version.DRAFT_16, Version.DRAFT_17] as const) {
		expect(sharedStreamCode(StreamCode.GoingAway, version)).toBe(false);
	}
	for (const version of [Version.DRAFT_18, Version.DRAFT_19, Version.DRAFT_20] as const) {
		expect(sharedStreamCode(StreamCode.GoingAway, version)).toBe(true);
	}

	expect(sharedStreamCode(StreamCode.TooFarBehind, Version.DRAFT_16)).toBe(false);
	expect(sharedStreamCode(StreamCode.TooFarBehind, Version.DRAFT_17)).toBe(true);
	expect(sharedStreamCode(StreamCode.MalformedTrack, Version.DRAFT_15)).toBe(false);
	expect(sharedStreamCode(StreamCode.MalformedTrack, Version.DRAFT_16)).toBe(true);
});
