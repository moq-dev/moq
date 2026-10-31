import { describe, expect, test } from "bun:test";
import { SessionCode, SessionError } from "../error.ts";
import { checkPeerRequestId } from "./session.ts";

function code(id: bigint, client: boolean): SessionCode | undefined {
	try {
		checkPeerRequestId(id, client);
	} catch (err) {
		if (err instanceof SessionError) return err.code;
		throw err;
	}
	return undefined;
}

describe("peer request IDs", () => {
	test.each([false, true])("accept any order and reuse, but only the peer's parity, client=%s", (client) => {
		const first = client ? 1n : 0n;
		for (const offset of [4n, 0n, 2n, 4n]) expect(code(first + offset, client)).toBeUndefined();
		expect(code(first ^ 1n, client)).toBe(SessionCode.InvalidRequestId);
	});

	test("IDs past 2^62 are refused", () => {
		expect(code((1n << 62n) - 2n, false)).toBeUndefined();
		for (const id of [1n << 62n, (1n << 64n) - 2n]) expect(code(id, false)).toBe(SessionCode.InvalidRequestId);
	});
});
