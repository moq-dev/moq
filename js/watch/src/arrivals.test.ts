import { expect, test } from "bun:test";
import { Milli } from "@moq/clock";
import { Producer } from "./arrivals";

const frame = (arrival: number) => ({
	kind: "frame" as const,
	timestamp: Milli(arrival),
	arrival: Milli(arrival),
	late: false,
});

test("a handle receives every frame written while it is open", async () => {
	const producer = new Producer();
	producer.write(frame(0));
	const handle = producer.consume();
	for (let i = 1; i <= 3; i++) producer.write(frame(i));
	for (let i = 1; i <= 3; i++) expect(await handle.next()).toEqual(frame(i));

	const waiting = handle.next();
	producer.write(frame(4));
	expect(await waiting).toEqual(frame(4));
	handle.close();
});

test("closing a handle releases its unread frames and stops observing", async () => {
	const producer = new Producer();
	const closed = producer.consume();
	const open = producer.consume();
	producer.write(frame(0));
	closed.close();
	producer.write(frame(1));
	expect(await closed.next()).toBeUndefined();
	expect(await open.next()).toEqual(frame(0));
	expect(await open.next()).toEqual(frame(1));
	open.close();
});

test("a closed decoder ends its handles once they drain", async () => {
	const producer = new Producer();
	const handle = producer.consume();
	producer.write(frame(0));
	producer.close();
	expect(await handle.next()).toEqual(frame(0));
	expect(await handle.next()).toBeUndefined();
	expect(await producer.consume().next()).toBeUndefined();
});

test("a pending read refuses a concurrent one", async () => {
	const producer = new Producer();
	const handle = producer.consume();
	const waiting = handle.next();
	await expect(handle.next()).rejects.toThrow("multiple calls to next not supported");
	// Woken but not yet resumed: the queued frame still belongs to the pending read.
	producer.write(frame(0));
	await expect(handle.next()).rejects.toThrow("multiple calls to next not supported");
	expect(await waiting).toEqual(frame(0));
	handle.close();
});

test("leaving a for-await loop closes the handle", async () => {
	const producer = new Producer();
	const handle = producer.consume();
	producer.write(frame(0));
	for await (const entry of handle) {
		expect(entry).toEqual(frame(0));
		break;
	}
	producer.write(frame(1));
	expect(await handle.next()).toBeUndefined();
});
