import { expect, spyOn, test } from "bun:test";
import { Milli } from "@moq/clock";
import { Time, Track, Varint } from "@moq/net";
import { Signal } from "@moq/signals";
import { Consumer } from "./consumer.ts";
import { Format } from "./legacy.ts";

const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

for (const count of [1, 16, 256]) {
	test(`Consumer summarizes ${count} skipped groups once per catch-up`, async () => {
		const warn = spyOn(console, "warn").mockImplementation(() => {});
		const track = new Track.Producer("tone");
		const delay = new Signal<Track.Delay>({ max: Milli(30_000) });
		const consumer = new Consumer(track.subscribe({ delay: { max: Milli(30_000) } }), {
			format: new Format("data"),
			delay,
		});
		const write = (sequence: bigint, timestamp: number) => {
			const group = track.createGroup(sequence);
			group.writeFrame({ payload: Varint.encode(timestamp), timestamp: Time.Timestamp.now() });
			return group;
		};
		const buffered = async (timestamp: number) => {
			while (consumer.buffered.peek().at(-1)?.end !== timestamp) await consumer.buffered.changed();
		};
		try {
			// Sparse sequence numbers make the count differ from the sequence range.
			for (let i = 0; i < count; i++) write(BigInt(i) * 2n, i * 1000);
			await buffered(count - 1);
			expect(warn).not.toHaveBeenCalled();
			delay.set({ max: Milli.zero });
			const newest = write(BigInt(count) * 2n, count * 1000);
			await buffered(count);
			expect(warn).toHaveBeenCalledTimes(1);
			expect(warn).toHaveBeenLastCalledWith(`skipping slow groups: track=tone 0 -> ${count * 2} count=${count}`);
			expect(consumer.buffered.peek()[0]?.start).toBe(Milli(count));
			// The retained group is still delivered, and another catch-up reports separately.
			expect(await consumer.next()).toMatchObject({ kind: "frame", group: BigInt(count) * 2n });
			newest.writeFrame({ payload: Varint.encode(count * 1000), timestamp: Time.Timestamp.now() });
			write(BigInt(count) * 2n + 2n, (count + 1) * 1000);
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
		const consumer = new Consumer(track.subscribe({ delay: { max: Milli(30_000) } }), {
			format: new Format("data"),
			delay: { max: Milli(1500) },
		});
		const frame = (micros: number) => ({ payload: Varint.encode(micros), timestamp: Time.Timestamp.now() });
		try {
			for (let g = 0n; g < 5n; g++) {
				const group = track.createGroup(g);
				for (let f = 0; f < 10; f++) group.writeFrame(frame(Number(g) * 1_000_000 + f * 100_000));
				group.finish();
			}
			const last = track.createGroup(5n);
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
	const consumer = new Consumer(track.subscribe({ delay: { max: Milli(30_000) } }), {
		format: new Format("data"),
		delay: { max: Milli(1500) },
	});
	const frame = (micros: number) => ({ payload: Varint.encode(micros), timestamp: Time.Timestamp.now() });
	const frames = (g: bigint) => Array.from({ length: 10 }, (_, f) => Number(g) * 1_000_000 + f * 100_000);
	const read: number[] = [];
	const readN = async (n: number) => {
		for (let i = 0; i < n; i++) {
			const next = await consumer.next();
			if (next?.kind !== "frame") throw new Error(`unexpected read: ${JSON.stringify(next)}`);
			read.push(next.frame.timestamp);
		}
	};
	try {
		for (let g = 0n; g < 4n; g++) {
			const group = track.createGroup(g);
			for (const micros of frames(g)) group.writeFrame(frame(micros));
			group.finish();
		}
		const fifth = track.createGroup(4n);
		// The fifth group's frames left before the sixth's, though they land after it.
		const sent = Time.Timestamp.now();
		const last = track.createGroup(5n);
		last.writeFrame(frame(5_000_000));
		last.finish();
		while (consumer.buffered.peek().at(-1)?.end !== 5000) await consumer.buffered.changed();

		await readN(10);
		for (const micros of frames(4n)) fifth.writeFrame({ payload: Varint.encode(micros), timestamp: sent });
		fifth.finish();
		await readN(11);
		expect(read).toEqual([...frames(3n), ...frames(4n), 5_000_000]);
	} finally {
		consumer.close();
		track.finish();
		track.close();
		warn.mockRestore();
	}
});

test("Consumer measures each skip's span over every group left, not just the new oldest", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const track = new Track.Producer("tone");
	const delay = new Signal<Track.Delay>({ max: Milli(30_000) });
	const consumer = new Consumer(track.subscribe({ delay: { max: Milli(30_000) } }), {
		format: new Format("data"),
		delay,
	});
	const write = (sequence: bigint, timestamp: number) => {
		const group = track.createGroup(sequence);
		group.writeFrame({ payload: Varint.encode(timestamp), timestamp: Time.Timestamp.now() });
	};
	const buffered = async (timestamp: number) => {
		while (consumer.buffered.peek().at(-1)?.end !== timestamp) await consumer.buffered.changed();
	};
	try {
		// Group 2 starts below group 1, so the span after skipping group 0 still reaches back to it.
		for (const [sequence, timestamp] of [
			[0n, 0],
			[1n, 10_000],
			[2n, 1_000],
			[3n, 12_000],
		] as const)
			write(sequence, timestamp);
		await buffered(12);
		// Before the first read, the span alone ages a group out: once reading began, its successor's
		// start bounds it instead, which the span always reaches past.
		delay.set({ max: Milli(5) });
		write(4n, 13_000);
		await buffered(13);
		expect(warn).toHaveBeenLastCalledWith("skipping slow groups: track=tone 0 -> 3 count=3");
		expect(consumer.buffered.peek()).toEqual([
			{ start: Milli(12), end: Milli(12) },
			{ start: Milli(13), end: Milli(13) },
		]);
		// Joining mid-track is not a gap: nothing was read before it.
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 3n });
	} finally {
		consumer.close();
		track.finish();
		track.close();
		warn.mockRestore();
	}
});

// An open group blocks until its successor starts a whole budget behind the newest content, not until
// its own first frame does: the skip moves the cursor past the groups still in flight between, which
// a group arriving below the cursor never delivers.
test("a blocking group is kept until its successor starts a budget behind the newest", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const track = new Track.Producer("tone");
	const consumer = new Consumer(track.subscribe({ delay: { max: Milli(30_000) } }), {
		format: new Format("data"),
		delay: { max: Milli(150) },
	});
	const sent = Time.Timestamp.now();
	const write = (sequence: bigint, timestamp: number) => {
		const group = track.createGroup(sequence);
		group.writeFrame({ payload: Varint.encode(timestamp), timestamp: sent });
		return group;
	};
	try {
		const blocking = write(0n, 0);
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 0n });
		// Group 3's stream wins the race, 160ms past group 0's first frame.
		write(3n, 160_000).finish();
		await flush();
		write(1n, 100_000).finish();
		write(2n, 130_000).finish();
		blocking.finish();
		const reads = [await consumer.next(), await consumer.next(), await consumer.next()];
		expect(reads).toMatchObject([
			{ kind: "frame", group: 1n },
			{ kind: "frame", group: 2n },
			{ kind: "frame", group: 3n },
		]);
		expect(warn).not.toHaveBeenCalled();
	} finally {
		consumer.close();
		track.finish();
		track.close();
		warn.mockRestore();
	}
});
