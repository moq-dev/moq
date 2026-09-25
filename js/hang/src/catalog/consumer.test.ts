import { expect, test } from "bun:test";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { TRACK } from "./format";
import { checkRenditions, MAX_RENDITIONS, type Root, TooManyRenditions, watch } from "./root";

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
