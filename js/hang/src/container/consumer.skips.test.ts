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

// Pairs `rs/moq-mux`'s `a_buffered_backlog_starts_within_the_budget`: a backlog that lands whole before
// the first read starts at the first group whose first frame is within the budget of the newest one.
// Five one-second groups of ten frames and a one-frame sixth at 5 s, with a 1.5 s budget, start at the
// 4 s group, whether the sixth is open or finished.
for (const open of [true, false]) {
	test(`a buffered backlog starts within the budget (sixth group ${open ? "open" : "finished"})`, async () => {
		const warn = spyOn(console, "warn").mockImplementation(() => {});
		const track = new Track.Producer("backlog");
		const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
			format: new Format("data"),
			delay: { max: Milli(1500) },
		});
		const frame = (micros: number) => ({ payload: Varint.encode(micros), timestamp: Time.Timestamp.now() });
		try {
			for (let g = 0; g < 5; g++) {
				const group = track.createGroup(g);
				for (let f = 0; f < 10; f++) group.writeFrame(frame(g * 1_000_000 + f * 100_000));
				group.finish();
			}
			const last = track.createGroup(5);
			last.writeFrame(frame(5_000_000));
			if (!open) last.finish();
			while (consumer.buffered.peek().at(-1)?.end !== 5000) await consumer.buffered.changed();

			const read: number[] = [];
			for (let i = 0; i < 11; i++) {
				const next = await consumer.next();
				if (next?.kind !== "frame") throw new Error(`unexpected read: ${JSON.stringify(next)}`);
				read.push(next.frame.timestamp);
			}
			const expected = [...Array.from({ length: 10 }, (_, f) => 4_000_000 + f * 100_000), 5_000_000];
			expect(read).toEqual(expected);
		} finally {
			consumer.close();
			track.finish();
			track.close();
			warn.mockRestore();
		}
	});
}

// The start waits on the next group's first frame, which may land within the budget, rather than
// dropping that group. Here the fifth group (4 s) arrived with its frames still in flight, so the start
// keeps the fourth and plays on into the fifth. Rust's
// `a_backlog_start_waits_on_a_successor_without_frames` pairs this case.
test("a backlog start waits on a successor without frames", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const track = new Track.Producer("backlog");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		delay: { max: Milli(1500) },
	});
	const frame = (micros: number) => ({ payload: Varint.encode(micros), timestamp: Time.Timestamp.now() });
	const frames = (g: number) => Array.from({ length: 10 }, (_, f) => g * 1_000_000 + f * 100_000);
	const read: number[] = [];
	const readN = async (n: number) => {
		for (let i = 0; i < n; i++) {
			const next = await consumer.next();
			if (next?.kind !== "frame") throw new Error(`unexpected read: ${JSON.stringify(next)}`);
			read.push(next.frame.timestamp);
		}
	};
	try {
		for (let g = 0; g < 4; g++) {
			const group = track.createGroup(g);
			for (const micros of frames(g)) group.writeFrame(frame(micros));
			group.finish();
		}
		const fifth = track.createGroup(4);
		// The fifth group's frames left before the sixth's, though they land after it.
		const sent = Time.Timestamp.now();
		const last = track.createGroup(5);
		last.writeFrame(frame(5_000_000));
		last.finish();
		while (consumer.buffered.peek().at(-1)?.end !== 5000) await consumer.buffered.changed();

		await readN(10);
		for (const micros of frames(4)) fifth.writeFrame({ payload: Varint.encode(micros), timestamp: sent });
		fifth.finish();
		await readN(11);
		expect(read).toEqual([...frames(3), ...frames(4), 5_000_000]);
	} finally {
		consumer.close();
		track.finish();
		track.close();
		warn.mockRestore();
	}
});
