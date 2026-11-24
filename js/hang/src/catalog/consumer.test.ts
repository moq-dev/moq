import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { defined } from "../defined.test-util.ts";
import { TRACK } from "./format";
import {
	checkRenditions,
	checkResolvable,
	EscapingBroadcast,
	type Root,
	UnsupportedVersion,
	VERSION,
	watch,
} from "./root";

function catalog(count: number): Root {
	return {
		audio: {
			renditions: Object.fromEntries(
				Array.from({ length: count }, (_, i) => [
					`audio${i}`,
					{ codec: "opus", sampleRate: 48_000, numberOfChannels: 2, container: { kind: "legacy" } },
				]),
			),
		},
	} as Root;
}

test("the rendition count is the publisher's to choose", () => {
	expect(Object.keys(checkRenditions(catalog(1000)).audio?.renditions ?? {})).toHaveLength(1000);
});

test("the shared containment check covers every section carrying a broadcast reference", () => {
	// @moq/watch runs this same check on the hang, MSF, and manual catalogs, so a section left
	// out here would exempt its tracks everywhere.
	const base = Moq.Path.from("a/b");
	for (const [section, key] of [
		["video", "renditions"],
		["audio", "renditions"],
		["text", "renditions"],
		["json", "tracks"],
		["binary", "tracks"],
	]) {
		const reference = (broadcast: string) =>
			({ [section]: { [key]: { entry: { broadcast: Moq.Path.normalizeRelative(broadcast) } } } }) as Root;
		expect(() => checkResolvable(reference("../../x"), base)).toThrow(EscapingBroadcast);
		expect(checkResolvable(reference("../x"), base)).toBeDefined();
	}

	// An include is a whole broadcast, keyed directly under its section.
	const include = (broadcast: string) =>
		({ include: { entry: { broadcast: Moq.Path.normalizeRelative(broadcast) } } }) as Root;
	expect(() => checkResolvable(include("../../x"), base)).toThrow(EscapingBroadcast);
	expect(checkResolvable(include("../x"), base)).toBeDefined();
});

test("watch yields a catalog with many renditions", async () => {
	const broadcast = new Moq.Broadcast.Producer();
	const track = defined(broadcast.createTrack(TRACK, { timescale: Moq.Time.Timescale.MILLI }));
	const producer = new Json.Snapshot.Producer<Root>({ track, deltas: false });
	const consumer = watch(broadcast.consume())[Symbol.asyncIterator]();
	producer.update(catalog(1000), Moq.Time.Timestamp.now());
	const next = await consumer.next();
	expect(Object.keys(next.value?.value.audio?.renditions ?? {})).toHaveLength(1000);
	producer.finish();
	broadcast.close();
});

test("watch refuses a catalog needing a newer reader", async () => {
	const broadcast = new Moq.Broadcast.Producer();
	const track = defined(broadcast.createTrack(TRACK, { timescale: Moq.Time.Timescale.MILLI }));
	const producer = new Json.Snapshot.Producer<Root>({ track, deltas: false });
	const consumer = watch(broadcast.consume())[Symbol.asyncIterator]();
	producer.update({ version: VERSION + 1 }, Moq.Time.Timestamp.now());
	await expect(consumer.next()).rejects.toBeInstanceOf(UnsupportedVersion);
	producer.finish();
	broadcast.close();
});

// A catalog whose one audio rendition references `broadcast`.
function referencing(broadcast: string): Root {
	const root = catalog(1);
	const [config] = Object.values(root.audio?.renditions ?? {});
	if (config) config.broadcast = Moq.Path.normalizeRelative(broadcast);
	return root;
}

// Watch the catalog of `room/alice`, published through an origin and requested from it.
async function watchRequested(root: Root) {
	const origin = new Moq.Origin.Producer();
	const broadcastRoute = origin.route(Moq.Path.from("room/alice"), { hidden: true });
	const broadcast = broadcastRoute.broadcast;
	const track = defined(broadcast.createTrack(TRACK, { timescale: Moq.Time.Timescale.MILLI }));
	const producer = new Json.Snapshot.Producer<Root>({ track, deltas: false });
	broadcastRoute.announce();
	const request = origin.request(Moq.Path.from("room/alice"));
	const active = request.active.peek();
	if (!active) throw new Error("request did not resolve");
	const consumer = watch(active)[Symbol.asyncIterator]();
	producer.update(root, Moq.Time.Timestamp.now());
	try {
		return await consumer.next();
	} finally {
		await consumer.return?.();
		request.close();
		broadcast.close();
		origin.close();
	}
}

test("watch refuses a broadcast reference escaping the requested path", async () => {
	await expect(watchRequested(referencing("../../../other"))).rejects.toBeInstanceOf(EscapingBroadcast);
});

test("watch accepts a sibling reference under the same root and yields it unresolved", async () => {
	const root = referencing("./bob");
	expect(await watchRequested(root)).toMatchObject({ value: { value: root } });
});

test("watch on a standalone broadcast refuses any parent reference", async () => {
	const broadcast = new Moq.Broadcast.Producer();
	const track = defined(broadcast.createTrack(TRACK, { timescale: Moq.Time.Timescale.MILLI }));
	const producer = new Json.Snapshot.Producer<Root>({ track, deltas: false });
	const consumer = watch(broadcast.consume())[Symbol.asyncIterator]();
	producer.update(referencing("../bob"), Moq.Time.Timestamp.now());
	await expect(consumer.next()).rejects.toBeInstanceOf(EscapingBroadcast);
	producer.finish();
	broadcast.close();
});

test("watch yields each catalog update with the timestamp it takes effect at", async () => {
	const broadcast = new Moq.Broadcast.Producer();
	const track = defined(broadcast.createTrack(TRACK, { timescale: Moq.Time.Timescale.MILLI }));
	const producer = new Json.Snapshot.Producer<Root>({ track, deltas: false });
	const consumer = watch(broadcast.consume())[Symbol.asyncIterator]();
	for (const [count, ms] of [
		[1, 1_000],
		[2, 2_500],
	]) {
		producer.update(catalog(count), Moq.Time.Timestamp.fromMillis(ms));
		const next = await consumer.next();
		expect(next.value?.value).toEqual(catalog(count));
		expect(next.value?.at.asMillis()).toBe(ms);
	}
	await consumer.return?.();
	producer.finish();
	broadcast.close();
});

test("a holding watcher reads every update it may still play, in order", async () => {
	// A reader `delay` behind the live edge (3s) plays from `3000 - delay`, so it needs the update in
	// effect there and every later one; without a delay it takes only the newest.
	for (const [delay, expected] of [
		[undefined, [3000]],
		[Duration.millis(999), [2000, 3000]],
		[Duration.millis(1001), [1000, 2000, 3000]],
	] as const) {
		const broadcast = new Moq.Broadcast.Producer();
		const track = defined(
			broadcast.createTrack(TRACK, { timescale: Moq.Time.Timescale.MILLI, maxAge: Duration.secs(30) }),
		);
		const producer = new Json.Snapshot.Producer<Root>({ track, deltas: false });
		// Subscribed before the updates land, as a live reader is; all three arrive before it reads.
		const updates = watch(broadcast.consume(), { delay })[Symbol.asyncIterator]();
		const first = updates.next();
		for (const ms of [1000, 2000, 3000]) producer.update(catalog(ms / 1000), Moq.Time.Timestamp.fromMillis(ms));
		producer.finish();
		const read: number[] = [];
		for (let next = await first; !next.done; next = await updates.next()) read.push(next.value.at.asMillis());
		expect(read).toEqual([...expected]);
		broadcast.close();
	}
});

test("echo feedback prefix refuses root escape with the same URL resolution as renditions", () => {
	for (const [base, reference, expected] of [
		["room/live", "viewers", "room/viewers"],
		["room/live", "live/viewers", "room/live/viewers"],
		["room/live", "../viewers", "viewers"],
		["room/live", "../../viewers", undefined],
		["live", "../viewers", undefined],
	] as const) {
		const root = { echo: { path: Moq.Path.normalizeRelative(reference) } };
		const path = Moq.Path.from(base);
		if (expected === undefined) expect(() => checkResolvable(root, path)).toThrow(EscapingBroadcast);
		else {
			expect(checkResolvable(root, path)).toEqual(root);
			expect(Moq.Path.tryResolve(path, root.echo.path)).toBe(Moq.Path.from(expected));
		}
	}
});

test("watch refuses an echo prefix escaping the requested broadcast root", async () => {
	await expect(
		watchRequested({ echo: { path: Moq.Path.normalizeRelative("../../viewers") } }),
	).rejects.toBeInstanceOf(EscapingBroadcast);
	const root = { echo: { path: Moq.Path.normalizeRelative("viewers") } };
	expect(await watchRequested(root)).toMatchObject({ value: { value: root } });
});
