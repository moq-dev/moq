import { expect, test } from "bun:test";
import { fromRequestCode, type RequestCondition, type RequestKind, toRequestCode } from "./error.ts";
import { type IetfVersion, Version } from "./version.ts";

const ALL: IetfVersion[] = [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22];

const KINDS: RequestKind[] = [
	"subscribe",
	"fetch",
	"publish",
	"publish_namespace",
	"subscribe_namespace",
	"track_status",
];

/** Every condition a rejection can carry. A new one belongs here. */
const CONDITIONS: RequestCondition[] = [
	"internal",
	"unauthorized",
	"expired",
	"timeout",
	"not_supported",
	"does_not_exist",
	"invalid_range",
	"invalid_filter",
	"invalid_joining_request_id",
	"uninterested",
	"malformed_track",
	"going_away",
];

/**
 * Every code we put in a rejection has to be one the drafts register. The table is
 * transcribed from draft-20 section 15.11.2, not derived from the mapping, so a mistake in
 * the mapping cannot talk the assertion into agreeing with it.
 */
const REGISTERED = [
	0x0, 0x1, 0x2, 0x3, 0x4, 0x5, 0x6, 0x9, 0x10, 0x11, 0x12, 0x19, 0x20, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36,
];

test("fetch refusal codes follow each draft registry", () => {
	for (const version of ALL) {
		const range = 0x11;
		expect(toRequestCode("invalid_range", "fetch", version)).toBe(range);
		expect(fromRequestCode(range, "fetch", version)).toBe("invalid_range");
		const joining = version <= Version.DRAFT_18 ? 0x32 : undefined;
		expect(toRequestCode("invalid_joining_request_id", "fetch", version)).toBe(joining ?? 0);
		if (joining !== undefined)
			expect(fromRequestCode(joining, "fetch", version)).toBe("invalid_joining_request_id");
		else expect(fromRequestCode(0x32, "fetch", version)).toBeUndefined();
	}
});

test("only registered codes reach the wire", () => {
	for (const version of ALL) {
		for (const kind of KINDS) {
			// Sent but never read back as itself, so it stays out of the round trip.
			for (const condition of [...CONDITIONS, "malformed_auth_token" as const]) {
				const code = toRequestCode(condition, kind, version);
				expect(REGISTERED).toContain(code);
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

test("going away is 0x6 on every request", () => {
	for (const kind of KINDS) {
		for (const version of ALL) {
			expect(toRequestCode("going_away", kind, version)).toBe(0x6);
			expect(fromRequestCode(0x6, kind, version)).toBe("going_away");
		}
	}
});

test("INVALID_FILTER is registered from draft 20", () => {
	for (const version of ALL) {
		expect(toRequestCode("invalid_filter", "subscribe", version)).toBe(version >= Version.DRAFT_20 ? 0x36 : 0);
		expect(fromRequestCode(0x36, "fetch", version)).toBe(
			version >= Version.DRAFT_20 ? "invalid_filter" : undefined,
		);
	}
});
