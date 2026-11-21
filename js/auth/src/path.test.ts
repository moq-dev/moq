import { expect, test } from "bun:test";
import { Path as NetPath } from "@moq/net";
import * as Path from "./path.ts";

// The signer and `@moq/net`'s `Path.from` agree on every root spelled with `a`, `/`, and `\`
// up to seven characters, so a token is never minted for a root a session cannot dial, nor
// refused for one it can.
test("signable matches Path.from", () => {
	let roots = [""];
	const all = [""];
	for (let length = 1; length <= 7; length++) {
		roots = roots.flatMap((root) => ["a", "/", "\\"].map((c) => root + c));
		all.push(...roots);
	}
	expect(all.length).toBe(3280);
	const accepts = (check: () => unknown) => {
		try {
			check();
			return true;
		} catch {
			return false;
		}
	};
	for (const root of all) {
		expect([root, accepts(() => Path.signable(root))]).toEqual([root, accepts(() => NetPath.from(root))]);
	}
});
