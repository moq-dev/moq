import { expect, spyOn, test } from "bun:test";
import { Milli } from "@moq/clock";
import { Time, Track, Varint } from "@moq/net";
import { Signal } from "@moq/signals";
import { Consumer, type Delay } from "./consumer.ts";
import { Format } from "./legacy.ts";

for (const count of [1, 16, 256]) {
	test(`Consumer summarizes ${count} skipped groups once per catch-up`, async () => {
		const warn = spyOn(console, "warn").mockImplementation(() => {});
		const track = new Track.Producer("tone");
		const delay = new Signal<Delay>({ max: Milli(30_000) });
		const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
			format: new Format("data"),
			delay,
		});
		const write = (sequence: number, timestamp: number) => {
			const group = track.createGroup(sequence);
			group.writeFrame({ payload: Varint.encode(timestamp), timestamp: Time.Timestamp.now() });
			return group;
		};
		const buffered = async (timestamp: number) => {
			while (consumer.buffered.peek().at(-1)?.end !== timestamp) await consumer.buffered.changed();
		};
		try {
			// Sparse sequence numbers make the count differ from the sequence range.
			for (let i = 0; i < count; i++) write(i * 2, i * 1000);
			await buffered(count - 1);
			expect(warn).not.toHaveBeenCalled();
			delay.set({ max: Milli.zero });
			const newest = write(count * 2, count * 1000);
			await buffered(count);
			expect(warn).toHaveBeenCalledTimes(1);
			expect(warn).toHaveBeenLastCalledWith(`skipping slow groups: track=tone 0 -> ${count * 2} count=${count}`);
			expect(consumer.buffered.peek()[0]?.start).toBe(Milli(count));
			// The retained group is still delivered, and another catch-up reports separately.
			expect(await consumer.next()).toMatchObject({ kind: "frame", group: count * 2 });
			newest.writeFrame({ payload: Varint.encode(count * 1000), timestamp: Time.Timestamp.now() });
			write(count * 2 + 2, (count + 1) * 1000);
			await buffered(count + 1);
			expect(warn).toHaveBeenCalledTimes(2);
			expect(warn).toHaveBeenLastCalledWith(
				`skipping slow groups: track=tone ${count * 2} -> ${count * 2 + 2} count=1`,
			);
		} finally {
			consumer.close();
			track.finish();
			track.close();
			warn.mockRestore();
		}
	});
}

test("Consumer measures each skip's span over every group left, not just the new oldest", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const track = new Track.Producer("tone");
	const maxDelay = new Signal(Milli(30_000));
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		maxDelay,
	});
	const write = (sequence: number, timestamp: number) => {
		const group = track.createGroup(sequence);
		group.writeFrame({ payload: Varint.encode(timestamp), timestamp: Time.Timestamp.now() });
	};
	const buffered = async (timestamp: number) => {
		while (consumer.buffered.peek().at(-1)?.end !== timestamp) await consumer.buffered.changed();
	};
	try {
		// Group 2 starts below group 1, so the span after skipping group 0 still reaches back to it.
		for (const [sequence, timestamp] of [
			[0, 0],
			[1, 10_000],
			[2, 1_000],
			[3, 12_000],
		])
			write(sequence, timestamp);
		await buffered(12);
		maxDelay.set(Milli(5));
		write(4, 13_000);
		await buffered(13);
		expect(warn).toHaveBeenLastCalledWith("skipping slow groups: track=tone 0 -> 3 count=3");
		expect(consumer.buffered.peek()).toEqual([
			{ start: Milli(12), end: Milli(12) },
			{ start: Milli(13), end: Milli(13) },
		]);
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 3 });
	} finally {
		consumer.close();
		track.finish();
		track.close();
		warn.mockRestore();
	}
});
