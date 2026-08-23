import { expect, test } from "bun:test";
import { DuplicateTrackAlias, RetiredTrackAlias, SharedTrackAlias, TrackAliases } from "./aliases.ts";

test("waits for the control message that establishes an alias", async () => {
	const aliases = new TrackAliases<object>();
	const track = {};
	const pending = aliases.get(7n);

	aliases.set(7n, track, { broadcast: "cam", name: "video" });

	expect(await pending).toBe(track);
});

test("resolves every subgroup waiting for the same alias", async () => {
	const aliases = new TrackAliases<object>();
	const track = {};
	const pending = [aliases.get(7n), aliases.get(7n)];

	aliases.set(7n, track, { broadcast: "cam", name: "video" });

	expect(await Promise.all(pending)).toEqual([track, track]);
});

test("rejects an alias used by two active tracks", () => {
	const aliases = new TrackAliases<object>();
	aliases.set(7n, {}, { broadcast: "cam", name: "video" });

	expect(() => aliases.set(7n, {}, { broadcast: "cam", name: "audio" })).toThrow(DuplicateTrackAlias);
});

// A track name may contain the separator a broadcast path uses, so identity has to keep the
// two apart: "a" + "b/c" and "a/b" + "c" are different tracks, and a publisher pointing one
// live alias at both is the collision section 11.1 makes fatal, not legal sharing.
test("a track name containing a slash does not collide with a longer namespace", () => {
	const aliases = new TrackAliases<object>();
	aliases.set(7n, {}, { broadcast: "a", name: "b/c" });

	expect(() => aliases.set(7n, {}, { broadcast: "a/b", name: "c" })).toThrow(DuplicateTrackAlias);
});

// Draft-19 section 5.1 lets a publisher give several subscriptions to one track the same
// alias. We cannot demux that, but it is a legal choice: failing the session over it would
// drop every other broadcast on the connection.
test("sharing an alias across subscriptions to one track is not fatal", () => {
	const aliases = new TrackAliases<object>();
	aliases.set(7n, {}, { broadcast: "cam", name: "video" });

	expect(() => aliases.set(7n, {}, { broadcast: "cam", name: "video" })).toThrow(SharedTrackAlias);
});

// Metadata keyed by an alias (the track's timescale) must only be torn down by whoever
// still owns the binding, so retirement reports ownership rather than returning void.
test("retire reports whether the caller still owned the alias", () => {
	const aliases = new TrackAliases<object>();
	const owner = {};
	aliases.set(7n, owner, { broadcast: "cam", name: "video" });

	expect(aliases.retire(7n, {})).toBe(false);
	expect(aliases.retire(7n, owner)).toBe(true);
});

test("does not let stale cleanup retire a reused alias", async () => {
	const aliases = new TrackAliases<object>();
	const active = {};
	aliases.set(7n, active, { broadcast: "cam", name: "video" });

	aliases.retire(7n, {});

	expect(await aliases.get(7n)).toBe(active);
});

// A cancelled subscription leaves its alias behind, so the groups the publisher is still
// sending are discarded at once instead of waiting out the timeout (draft-19 section 11.1).
test("a retired alias rejects late groups immediately", async () => {
	const aliases = new TrackAliases<object>();
	const track = {};
	aliases.set(7n, track, { broadcast: "cam", name: "video" });
	aliases.retire(7n, track);

	await expect(aliases.get(7n)).rejects.toBeInstanceOf(RetiredTrackAlias);
});

// The publisher may point a retired alias at a new track, so a later SUBSCRIBE_OK
// reclaims it rather than colliding with the tombstone.
test("a later subscribe reclaims a retired alias", async () => {
	const aliases = new TrackAliases<object>();
	const old = {};
	aliases.set(7n, old, { broadcast: "cam", name: "video" });
	aliases.retire(7n, old);

	const fresh = {};
	aliases.set(7n, fresh, { broadcast: "cam", name: "video" });

	expect(await aliases.get(7n)).toBe(fresh);
});

// Tombstones are bounded: a session churning through subscriptions must not accumulate
// one entry per alias it ever used.
test("retired aliases are capped", async () => {
	const aliases = new TrackAliases<object>();

	for (let i = 0n; i < 74n; i++) {
		const track = {};
		aliases.set(i, track, { broadcast: "cam", name: "video" });
		aliases.retire(i, track);
	}

	// The oldest tombstones are forgotten, so they fall back to the unknown-alias wait.
	await expect(aliases.get(0n)).rejects.toThrow("unknown track alias");
	await expect(aliases.get(73n)).rejects.toBeInstanceOf(RetiredTrackAlias);
});
