import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import { Reader } from "../stream.ts";
import * as Varint from "../varint.ts";
import { namespaceUpdate } from "./request_stream.ts";
import { Version } from "./version.ts";

test("an oversized or truncated namespace update type is a protocol violation", async () => {
	// Both drafts use leading-ones varints.
	for (const version of [Version.DRAFT_18, Version.DRAFT_21]) {
		const oversized = Varint.encodeLeadingOnes(2n ** 53n);
		// A two-byte varint prefix with its second byte missing.
		const truncated = Uint8Array.of(0x80);
		for (const bytes of [oversized, truncated]) {
			const reader = new Reader(undefined, bytes, version);
			await expect(namespaceUpdate(reader, version, { client: false, seen: false })).rejects.toBeInstanceOf(
				ProtocolViolation,
			);
		}
	}
});

test("namespace update framing refuses duplicate prefix parameters", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_21]) {
		const body = [2, 2, 0x34, 1, 1, 97, 0, 1, 1, 98];
		const reader = new Reader(undefined, Uint8Array.from([2, 0, body.length, ...body]), version);
		await expect(namespaceUpdate(reader, version, { client: false, seen: false })).rejects.toBeInstanceOf(
			ProtocolViolation,
		);
	}
});

test("a repeated namespace update token is a request refusal", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_21]) {
		const body = [2, 2, 3, 3, 3, 0, 170, 0, 3, 3, 0, 170];
		const reader = new Reader(undefined, Uint8Array.from([2, 0, body.length, ...body]), version);
		expect(await namespaceUpdate(reader, version, { client: false, seen: false })).toEqual({
			requestId: 2n,
			refused: "malformed_auth_token",
		});
	}
});
