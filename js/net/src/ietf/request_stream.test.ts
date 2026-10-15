import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import { Reader } from "../stream.ts";
import { namespaceUpdate } from "./request_stream.ts";
import { Version } from "./version.ts";

test("namespace update framing refuses duplicate prefix parameters", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_21]) {
		const body = [2, 2, 0x34, 1, 1, 97, 0, 1, 1, 98];
		const reader = new Reader(undefined, Uint8Array.from([2, 0, body.length, ...body]), version);
		await expect(namespaceUpdate(reader, version)).rejects.toBeInstanceOf(ProtocolViolation);
	}
});

test("a repeated namespace update token is a request refusal", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_21]) {
		const body = [2, 2, 3, 3, 3, 0, 170, 0, 3, 3, 0, 170];
		const reader = new Reader(undefined, Uint8Array.from([2, 0, body.length, ...body]), version);
		expect(await namespaceUpdate(reader, version)).toEqual({ requestId: 2n, refused: "malformed_auth_token" });
	}
});
