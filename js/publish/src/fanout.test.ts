import { describe, expect, test } from "bun:test";
import { Effect } from "@moq/signals";
import { Fanout } from "./fanout";

// A stream we can feed by hand, so a test controls exactly when values arrive.
function pushable<T>(): { stream: ReadableStream<T>; push: (value: T) => void; close: () => void } {
	let controller!: ReadableStreamDefaultController<T>;
	const stream = new ReadableStream<T>({
		start: (c) => {
			controller = c;
		},
	});
	return {
		stream,
		push: (value) => controller.enqueue(value),
		close: () => controller.close(),
	};
}

// Let the fanout's pump drain everything pushed so far, so a test that asserts on the queue's
// contents isn't racing it.
const flush = () => new Promise((resolve) => setTimeout(resolve, 10));

// Read up to `count` values, giving up once the stream goes quiet.
async function take<T>(stream: ReadableStream<T>, count: number): Promise<T[]> {
	const reader = stream.getReader();
	const out: T[] = [];

	for (let i = 0; i < count; i++) {
		const next = await Promise.race([
			reader.read(),
			new Promise<undefined>((resolve) => setTimeout(() => resolve(undefined), 50)),
		]);
		if (!next || next.done) break;
		out.push(next.value);
	}

	reader.releaseLock();
	return out;
}

describe("Fanout", () => {
	// The whole point: one source, several encoders, none of them stealing from the others.
	test("delivers every value to every reader", async () => {
		const source = pushable<number>();
		const fanout = new Fanout(source.stream);
		const effect = new Effect();

		const a = fanout.subscribe(effect);
		const b = fanout.subscribe(effect);

		source.push(1);
		source.push(2);
		source.push(3);

		expect(await take(a, 3)).toEqual([1, 2, 3]);
		expect(await take(b, 3)).toEqual([1, 2, 3]);

		effect.close();
		fanout.close();
	});

	// A second reader used to be impossible: pipeThrough/getReader locks the one stream.
	test("lets a reader attach after another is already reading", async () => {
		const source = pushable<number>();
		const fanout = new Fanout(source.stream);
		const effect = new Effect();

		const a = fanout.subscribe(effect);
		source.push(1);
		expect(await take(a, 1)).toEqual([1]);

		// Joins late, so it sees what comes next rather than the backlog.
		const b = fanout.subscribe(effect);
		source.push(2);

		expect(await take(a, 1)).toEqual([2]);
		expect(await take(b, 1)).toEqual([2]);

		effect.close();
		fanout.close();
	});

	// Head-of-line blocking would be worse than dropping: the source is a live capture.
	test("drops a slow reader's oldest without starving the others", async () => {
		const source = pushable<number>();
		const fanout = new Fanout(source.stream, { queue: 2 });
		const effect = new Effect();

		const slow = fanout.subscribe(effect);
		const fast = fanout.subscribe(effect);

		// Drain `fast` as we go; `slow` never reads, so it overflows its queue of 2.
		for (const value of [1, 2, 3, 4]) {
			source.push(value);
			expect(await take(fast, 1)).toEqual([value]);
		}

		// The newest two survive; the older two were dropped for this reader alone.
		expect(await take(slow, 2)).toEqual([3, 4]);

		effect.close();
		fanout.close();
	});

	test("stops delivering once its effect is torn down", async () => {
		const source = pushable<number>();
		const fanout = new Fanout(source.stream);
		const effect = new Effect();

		const stream = fanout.subscribe(effect);
		source.push(1);
		expect(await take(stream, 1)).toEqual([1]);

		effect.close();
		source.push(2);

		expect(await take(stream, 1)).toEqual([]);
		fanout.close();
	});

	// A reader closes what it receives, so handing the same VideoFrame to two renditions would
	// close it out from under the other. Content equality can't catch that; identity can.
	test("gives each reader its own copy", async () => {
		const source = pushable<{ id: number }>();
		const fanout = new Fanout(source.stream, {
			clone: (value) => ({ ...value }),
			release: () => {},
		});
		const effect = new Effect();

		const a = fanout.subscribe(effect);
		const b = fanout.subscribe(effect);

		source.push({ id: 1 });
		await flush();

		const [fromA] = await take(a, 1);
		const [fromB] = await take(b, 1);

		expect(fromA).toEqual({ id: 1 });
		expect(fromB).toEqual({ id: 1 });
		expect(fromA).not.toBe(fromB);

		effect.close();
		fanout.close();
	});

	// Everything the fanout discards has to be released, or a VideoFrame's buffers leak.
	test("releases what it discards", async () => {
		const source = pushable<{ id: number }>();
		const released: number[] = [];

		const fanout = new Fanout(source.stream, {
			queue: 1,
			clone: (value) => ({ ...value }),
			release: (value) => released.push(value.id),
		});
		const effect = new Effect();

		const stream = fanout.subscribe(effect);

		source.push({ id: 1 });
		source.push({ id: 2 });
		await flush();

		// One reader with a queue of 1: id 1 is dropped for it, and both originals are released
		// because each reader took a clone.
		expect(await take(stream, 1)).toEqual([{ id: 2 }]);

		// id 1 twice: the original the reader cloned from, plus the clone dropped from its queue.
		expect(released.filter((id) => id === 1).length).toBe(2);
		expect(released).toContain(2);

		effect.close();
		fanout.close();
	});

	test("releases values that arrive with nobody attached", async () => {
		const source = pushable<{ id: number }>();
		const released: number[] = [];

		const fanout = new Fanout(source.stream, { release: (value) => released.push(value.id) });

		source.push({ id: 7 });
		await flush();

		expect(released).toEqual([7]);
		fanout.close();
	});

	// Without this the pump just logged and stopped, leaving every reader parked on read() forever:
	// the encoders stalled with no terminal signal while still reporting themselves active.
	test("surfaces a source failure to every reader", async () => {
		let control!: ReadableStreamDefaultController<number>;
		const source = new ReadableStream<number>({
			start: (controller) => {
				control = controller;
			},
		});

		const fanout = new Fanout(source);
		const effect = new Effect();

		const a = fanout.subscribe(effect).getReader();
		const b = fanout.subscribe(effect).getReader();

		// Audio that already made it through is still delivered.
		control.enqueue(1);
		await flush();
		await expect(a.read().then((r) => r.value)).resolves.toBe(1);
		await expect(b.read().then((r) => r.value)).resolves.toBe(1);

		control.error(new Error("capture died"));
		await flush();

		// Both readers learn it died, rather than parking on a read that never settles.
		await expect(a.read()).rejects.toThrow("capture died");
		await expect(b.read()).rejects.toThrow("capture died");

		effect.close();
		fanout.close();
	});

	// FrameSource and SampleSource both document that their stream is cancelled when dropped, and a
	// one-shot decode keeps running (and holding the stream locked) if we don't.
	test("cancels its source on close", async () => {
		let cancelled = 0;
		const source = new ReadableStream<number>({
			cancel: () => {
				cancelled++;
			},
		});

		const fanout = new Fanout(source);
		await flush();

		fanout.close();
		await flush();
		expect(cancelled).toBe(1);

		// Idempotent: the pump also closes on end-of-stream, so this must not double-cancel.
		fanout.close();
		await flush();
		expect(cancelled).toBe(1);
	});

	test("closes its readers when the source ends", async () => {
		const source = pushable<number>();
		const fanout = new Fanout(source.stream);
		const effect = new Effect();

		const stream = fanout.subscribe(effect);
		source.push(1);
		source.close();

		const reader = stream.getReader();
		expect((await reader.read()).value).toBe(1);
		expect((await reader.read()).done).toBe(true);

		effect.close();
	});
});
