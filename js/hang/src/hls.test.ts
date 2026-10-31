import { expect, test } from "bun:test";
import type { Micro } from "@moq/clock";
import * as Json from "@moq/json";
import { Broadcast, Time } from "@moq/net";
import { u53 } from "./catalog";
import { Consumer, type Record } from "./hls.ts";

const us = (ms: number): Micro => (ms * 1000) as Micro;

const hls = { track: "video.hls", duration: u53(2000) };

function records() {
	const broadcast = new Broadcast.Producer();
	const track = broadcast.createTrack("video.hls", { timescale: Time.Timescale.MILLI });
	const producer = new Json.Window.Producer<Record>({ track });
	const consumer = Consumer.subscribe(broadcast.consume(), hls);
	const push = (value: Record) => producer.push({ value, at: Time.Timestamp.now() });
	const close = () => {
		producer.finish();
		consumer.close();
		broadcast.close();
	};
	return { producer, consumer, push, close };
}

test("the consumer yields records in microseconds, a stop included", async () => {
	const { producer, consumer, push, close } = records();
	push({ sequence: 0, group: 4, pts: 250 });
	push({ sequence: 1, group: 6, pts: 2250, duration: 0 });
	expect(await consumer.next()).toEqual({ push: { index: 0, entry: { sequence: 0, group: 4, pts: us(250) } } });
	expect(await consumer.next()).toEqual({
		push: { index: 1, entry: { sequence: 1, group: 6, pts: us(2250), duration: us(0) } },
	});
	producer.pop(1, Time.Timestamp.now());
	expect(await consumer.next()).toEqual({ pop: { start: 0, end: 1 } });
	close();
});

test("a record numbered unlike its window index is refused", async () => {
	const { consumer, push, close } = records();
	push({ sequence: 5, group: 0, pts: 0 });
	await expect(consumer.next()).rejects.toThrow("differs from its index");
	close();
});

test("a record without its group is refused", async () => {
	const { consumer, push, close } = records();
	push({ sequence: 0, pts: 0 } as Record);
	await expect(consumer.next()).rejects.toThrow();
	close();
});
