import { expect, test } from "bun:test";
import { RetiredTrackAlias, TrackAliases } from "./aliases.ts";

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

test("does not let stale cleanup retire a reused alias", async () => {
	const aliases = new TrackAliases<object>();
	const active = {};
	aliases.set(7n, active);

	aliases.retire(7n, {});

	expect(await aliases.get(7n)).toBe(active);
});

// A cancelled subscription leaves its alias behind, so the groups the publisher is still
// sending are discarded at once instead of waiting out the timeout (draft-19 section 11.1).
test("a retired alias rejects late groups immediately", async () => {
	const aliases = new TrackAliases<object>();
	const track = {};
	aliases.set(7n, track);
	aliases.retire(7n, track);

	await expect(aliases.get(7n)).rejects.toBeInstanceOf(RetiredTrackAlias);
});

// The publisher may point a retired alias at a new track, so a later SUBSCRIBE_OK
// reclaims it rather than colliding with the tombstone.
test("a later subscribe reclaims a retired alias", async () => {
	const aliases = new TrackAliases<object>();
	const old = {};
	aliases.set(7n, old);
	aliases.retire(7n, old);

	const fresh = {};
	aliases.set(7n, fresh);

	expect(await aliases.get(7n)).toBe(fresh);
});

// Tombstones are bounded: a session churning through subscriptions must not accumulate
// one entry per alias it ever used.
test("retired aliases are capped", async () => {
	const aliases = new TrackAliases<object>();

	for (let i = 0n; i < 74n; i++) {
		const track = {};
		aliases.set(i, track);
		aliases.retire(i, track);
	}

	// The oldest tombstones are forgotten, so they fall back to the unknown-alias wait.
	await expect(aliases.get(0n)).rejects.toThrow("unknown track alias");
	await expect(aliases.get(73n)).rejects.toBeInstanceOf(RetiredTrackAlias);
});
