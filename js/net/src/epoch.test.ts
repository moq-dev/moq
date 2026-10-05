import { expect, spyOn, test } from "bun:test";
import * as Epoch from "./epoch.ts";

test("mint uses UUIDv7 and orders successive identities with a fixed wall clock", () => {
	const now = spyOn(Date, "now").mockReturnValue(1_700_000_000_000);
	try {
		const epoch = Epoch.mint();
		expect(Epoch.parse(epoch)).toBe(epoch);
		expect(Epoch.time(epoch).getTime()).toBe(1_700_000_000_000);
		expect(Epoch.mint() > epoch).toBe(true);
	} finally {
		now.mockRestore();
	}
});
