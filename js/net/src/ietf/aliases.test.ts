import { expect, test } from "bun:test";
import { DuplicateTrackAlias, RetiredTrackAlias, SharedTrackAlias, TrackAliases } from "./aliases.ts";

test("waits for the control message that establishes an alias", async () => {
	const aliases = new TrackAliases<object>();
	const track = {};
	const pending = aliases.get(7n);

	aliases.set(7n, track, "cam/video");

	expect(await pending).toBe(track);
});

test("resolves every subgroup waiting for the same alias", async () => {
	const aliases = new TrackAliases<object>();
	const track = {};
	const pending = [aliases.get(7n), aliases.get(7n)];

	aliases.set(7n, track, "cam/video");

	expect(await Promise.all(pending)).toEqual([track, track]);
});

test("rejects an alias used by two active tracks", () => {
	const aliases = new TrackAliases<object>();
	aliases.set(7n, {}, "cam/video");

	expect(() => aliases.set(7n, {}, "cam/audio")).toThrow(DuplicateTrackAlias);
});

// Draft-19 section 5.1 lets a publisher give several subscriptions to one track the same
// alias. We cannot demux that, but it is a legal choice: failing the session over it would
// drop every other broadcast on the connection.
test("sharing an alias across subscriptions to one track is not fatal", () => {
	const aliases = new TrackAliases<object>();
	aliases.set(7n, {}, "cam/video");

	expect(() => aliases.set(7n, {}, "cam/video")).toThrow(SharedTrackAlias);
});

test("does not let stale cleanup retire a reused alias", async () => {
	const aliases = new TrackAliases<object>();
	const active = {};
	aliases.set(7n, active, "cam/video");

	aliases.retire(7n, {});

	expect(await aliases.get(7n)).toBe(active);
});

// A cancelled subscription leaves its alias behind, so the groups the publisher is still
// sending are discarded at once instead of waiting out the timeout (draft-19 section 11.1).
test("a retired alias rejects late groups immediately", async () => {
	const aliases = new TrackAliases<object>();
	const track = {};
	aliases.set(7n, track, "cam/video");
	aliases.retire(7n, track);

	await expect(aliases.get(7n)).rejects.toBeInstanceOf(RetiredTrackAlias);
});

// The publisher may point a retired alias at a new track, so a later SUBSCRIBE_OK
// reclaims it rather than colliding with the tombstone.
test("a later subscribe reclaims a retired alias", async () => {
	const aliases = new TrackAliases<object>();
	const old = {};
	aliases.set(7n, old, "cam/video");
	aliases.retire(7n, old);

	const fresh = {};
	aliases.set(7n, fresh, "cam/video");

	expect(await aliases.get(7n)).toBe(fresh);
});

// Tombstones are bounded: a session churning through subscriptions must not accumulate
// one entry per alias it ever used.
test("retired aliases are capped", async () => {
	const aliases = new TrackAliases<object>();

	for (let i = 0n; i < 74n; i++) {
		const track = {};
		aliases.set(i, track, "cam/video");
		aliases.retire(i, track);
	}

	// The oldest tombstones are forgotten, so they fall back to the unknown-alias wait.
	await expect(aliases.get(0n)).rejects.toThrow("unknown track alias");
	await expect(aliases.get(73n)).rejects.toBeInstanceOf(RetiredTrackAlias);
});
