import { expect } from "bun:test";
import type { Claim } from "./origin.ts";
import type * as Path from "./path.ts";

/** The claim among `claims` asked first, checking that it was asked for `path`, not merely closed. */
export async function askedFirst<T extends Claim>(path: Path.Valid, claims: readonly T[]): Promise<T> {
	const [claim, next] = await Promise.race(
		claims.map((claim) =>
			claim
				.requested()
				.next()
				.then((next) => [claim, next] as const),
		),
	);
	if (next.done) throw new Error("the claim closed without a request");
	expect(next.value.path).toBe(path);
	return claim;
}
