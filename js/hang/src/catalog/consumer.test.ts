import { expect, test } from "bun:test";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { TRACK } from "./format";
import { checkRenditions, EscapingBroadcast, MAX_RENDITIONS, type Root, TooManyRenditions, watch } from "./root";

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

test("the shared cap accepts 64 renditions and refuses 65 with a typed error", () => {
	expect(checkRenditions(catalog(MAX_RENDITIONS))).toBeDefined();
	expect(() => checkRenditions(catalog(MAX_RENDITIONS + 1))).toThrow(TooManyRenditions);
	expect(() =>
		checkRenditions({
			...catalog(MAX_RENDITIONS - 1),
			video: { renditions: { video: {} } },
			text: { renditions: { captions: {} } },
		} as unknown as Root),
	).toThrow(TooManyRenditions);
});

test("watch refuses an oversized catalog update", async () => {
	const broadcast = new Moq.Broadcast.Producer();
	const track = broadcast.createTrack(TRACK);
	const producer = new Json.Snapshot.Producer<Root>({ track, deltaRatio: 0 });
	const consumer = watch(broadcast.consume())[Symbol.asyncIterator]();
	producer.update(catalog(MAX_RENDITIONS + 1));
	await expect(consumer.next()).rejects.toBeInstanceOf(TooManyRenditions);
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
	const broadcast = origin.createBroadcast(Moq.Path.from("room/alice"));
	const track = broadcast.createTrack(TRACK);
	const producer = new Json.Snapshot.Producer<Root>({ track, deltaRatio: 0 });
	broadcast.announce();
	const request = origin.request(Moq.Path.from("room/alice"));
	const active = request.active.peek();
	if (!active) throw new Error("request did not resolve");
	const consumer = watch(active)[Symbol.asyncIterator]();
	producer.update(root);
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
	expect(await watchRequested(root)).toMatchObject({ value: root });
});

test("watch on a standalone broadcast refuses any parent reference", async () => {
	const broadcast = new Moq.Broadcast.Producer();
	const track = broadcast.createTrack(TRACK);
	const producer = new Json.Snapshot.Producer<Root>({ track, deltaRatio: 0 });
	const consumer = watch(broadcast.consume())[Symbol.asyncIterator]();
	producer.update(referencing("../bob"));
	await expect(consumer.next()).rejects.toBeInstanceOf(EscapingBroadcast);
	producer.finish();
	broadcast.close();
});

test("watch subscribes and yields typed catalog updates", async () => {
	const broadcast = new Moq.Broadcast.Producer();
	const track = broadcast.createTrack(TRACK);
	const producer = new Json.Snapshot.Producer<Root>({ track, deltaRatio: 0 });
	const consumer = watch(broadcast.consume())[Symbol.asyncIterator]();
	producer.update(catalog(1));
	expect(await consumer.next()).toMatchObject({ value: catalog(1) });
	await consumer.return?.();
	producer.finish();
	broadcast.close();
});
