import { expect, test } from "bun:test";
import { Queue } from "./queue.ts";

test("Queue stays FIFO across interleaved pushes, shifts, and compactions", () => {
	const queue = new Queue<number>();
	const model: number[] = [];
	let next = 0;
	for (let round = 0; round < 50; round++) {
		for (let i = 0; i < round % 7; i++) {
			queue.push(next);
			model.push(next++);
		}
		for (let i = 0; i < round % 5; i++) expect(queue.shift()).toBe(model.shift());
		expect(queue.length).toBe(model.length);
		expect([...queue]).toEqual(model);
	}
	queue.clear();
	expect(queue.length).toBe(0);
	expect(queue.shift()).toBeUndefined();
});
