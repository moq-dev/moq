import { expect, test } from "bun:test";
import { TrackAliases } from "./aliases.ts";

test("waits for the control message that establishes an alias", async () => {
	const aliases = new TrackAliases<object>();
	const track = {};
	const pending = aliases.get(7n);

	aliases.set(7n, track);

	expect(await pending).toBe(track);
});

test("resolves every subgroup waiting for the same alias", async () => {
	const aliases = new TrackAliases<object>();
	const track = {};
	const pending = [aliases.get(7n), aliases.get(7n)];

	aliases.set(7n, track);

	expect(await Promise.all(pending)).toEqual([track, track]);
});

test("rejects an alias used by two active tracks", () => {
	const aliases = new TrackAliases<object>();
	aliases.set(7n, {});

	expect(() => aliases.set(7n, {})).toThrow("duplicate track alias");
});

test("does not let stale cleanup remove a reused alias", async () => {
	const aliases = new TrackAliases<object>();
	const active = {};
	aliases.set(7n, active);

	aliases.delete(7n, {});

	expect(await aliases.get(7n)).toBe(active);
});
