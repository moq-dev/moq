import { describe, expect, test } from "bun:test";
import { SessionCode, SessionError } from "../error.ts";
import { SpentIds } from "./adapter.ts";

function code(ids: SpentIds, id: bigint): SessionCode | undefined {
	try {
		ids.spend(id);
	} catch (err) {
		if (err instanceof SessionError) return err.code;
		throw err;
	}
	return undefined;
}

describe("draft-17+ request ID history", () => {
	test.each([0n, 1n])("allows reordering but permanently spends IDs, first=%s", (first) => {
		const ids = new SpentIds(first);
		for (const offset of [4n, 0n, 2n, 8n, 6n]) ids.spend(first + offset);
		for (const offset of [0n, 2n, 4n, 6n, 8n]) expect(code(ids, first + offset)).toBe(SessionCode.InvalidRequestId);
		expect(code(ids, first ^ 1n)).toBe(SessionCode.InvalidRequestId);
	});

	test("contiguous traffic in either order leaves no gap", () => {
		const forward = new SpentIds(0n, 0);
		const reverse = new SpentIds(0n, 1);
		for (let i = 0n; i < 4096n; i++) {
			forward.spend(i * 2n);
			reverse.spend((4095n - i) * 2n);
		}
		expect(code(forward, 0n)).toBe(SessionCode.InvalidRequestId);
		expect(code(reverse, 8190n)).toBe(SessionCode.InvalidRequestId);
	});

	test("a full history refuses a new gap without forgetting IDs", () => {
		const ids = new SpentIds(0n, 1);
		ids.spend(0n);
		ids.spend(4n);
		expect(code(ids, 8n)).toBe(SessionCode.Internal);
		ids.spend(2n);
		ids.spend(8n);
		for (const id of [0n, 2n, 4n, 8n]) expect(code(ids, id)).toBe(SessionCode.InvalidRequestId);
	});

	test("a huge sparse ID does not allocate its intervening gap", () => {
		const ids = new SpentIds(0n, 1);
		ids.spend((1n << 62n) - 2n);
		ids.spend(0n);
		for (const id of [(1n << 62n) - 2n, 1n << 62n, (1n << 64n) - 2n]) {
			expect(code(ids, id)).toBe(SessionCode.InvalidRequestId);
		}
	});
});
