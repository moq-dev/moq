import { describe, expect, test } from "bun:test";
import { SessionCode, SessionError } from "../error.ts";
import { RequestIds } from "./request.ts";

function code(ids: RequestIds, id: bigint): SessionCode | undefined {
	try {
		ids.accept(id);
	} catch (err) {
		if (err instanceof SessionError) return err.code;
		throw err;
	}
	return undefined;
}

describe("modern request ID history", () => {
	test.each([true, false])("allows reordering but permanently spends IDs, local client=%s", (client) => {
		const first = client ? 1n : 0n;
		const ids = new RequestIds(client);
		for (const offset of [4n, 0n, 2n, 8n, 6n]) ids.accept(first + offset);
		for (const offset of [0n, 2n, 4n, 6n, 8n]) expect(code(ids, first + offset)).toBe(SessionCode.InvalidRequestId);
		expect(code(ids, first ^ 1n)).toBe(SessionCode.InvalidRequestId);
	});
	test("contiguous traffic and reverse arrival use one range", () => {
		const forward = new RequestIds(false, 1);
		const reverse = new RequestIds(false, 1);
		for (let i = 0n; i < 4096n; i++) {
			forward.accept(i * 2n);
			reverse.accept((4095n - i) * 2n);
		}
		expect(code(forward, 0n)).toBe(SessionCode.InvalidRequestId);
		expect(code(reverse, 8190n)).toBe(SessionCode.InvalidRequestId);
	});
	test("bridging ranges frees capacity without forgetting IDs", () => {
		const ids = new RequestIds(false, 2);
		ids.accept(0n);
		ids.accept(4n);
		expect(code(ids, 8n)).toBe(SessionCode.Internal);
		ids.accept(2n);
		ids.accept(8n);
		for (const id of [0n, 2n, 4n, 8n]) expect(code(ids, id)).toBe(SessionCode.InvalidRequestId);
	});
	test("a huge sparse ID does not allocate its intervening gap", () => {
		const ids = new RequestIds(false, 2);
		ids.accept((1n << 62n) - 2n);
		ids.accept(0n);
		expect(code(ids, (1n << 62n) - 2n)).toBe(SessionCode.InvalidRequestId);
		expect(code(ids, 1n << 62n)).toBe(SessionCode.InvalidRequestId);
	});
});
