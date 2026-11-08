import { expect, test } from "bun:test";
import { Manual, Micro, Milli, Nano } from "@moq/clock";
import { Time, Track } from "@moq/net";
import { Consumer, type Read } from "./consumer.ts";
import { encodeFrame, Format } from "./legacy.ts";

const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

test.each([0, 100])("a sequence hole before a paused data marker expires at %sms", async (budget) => {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("data");
	const consumer = new Consumer(track.subscribe({ delay: { max: Milli(30_000) } }), {
		format: new Format("data"),
		delay: { max: Milli(budget) },
	});
	const results: Read[] = [];
	const reading = (async () => {
		for (;;) {
			const read = await consumer.next();
			if (!read) return;
			results.push(read);
		}
	})();
	try {
		const first = track.createGroup(0n);
		first.writeFrame({ payload: encodeFrame(new Uint8Array([1]), Micro(10_000)), timestamp: Time.Timestamp.now() });
		first.finish();
		await flush();
		expect(results).toHaveLength(1);
		const marker = track.createGroup(2n);
		marker.writeFrame({ payload: new Uint8Array(), timestamp: Time.Timestamp.now() });
		marker.finish();
		await flush();
		if (budget > 0) {
			expect(results).toHaveLength(1);
			await clock.advance(Nano.fromMilli(Milli(budget - 1)));
			expect(results).toHaveLength(1);
			await clock.advance(Nano.fromMilli(Milli(1)));
		}
		expect(results.slice(1)).toEqual([
			{ kind: "gap", timestamp: Micro(10_000), duration: Micro.zero },
			{ kind: "end", group: 2n, end: Micro(10_000) },
		]);
	} finally {
		consumer.close();
		track.finish();
		track.close();
		try {
			await reading;
		} finally {
			installed.close();
		}
	}
});

test("a filled hole cancels its timer and the next hole gets a fresh budget", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("data");
	const consumer = new Consumer(track.subscribe({ delay: { max: Milli(30_000) } }), {
		format: new Format("data"),
		delay: { max: Milli(100) },
	});
	const results: Read[] = [];
	const reading = (async () => {
		for (;;) {
			const read = await consumer.next();
			if (!read) return;
			results.push(read);
		}
	})();
	const write = (sequence: bigint, timestamp?: number) => {
		const group = track.createGroup(sequence);
		group.writeFrame({
			payload: timestamp === undefined ? new Uint8Array() : encodeFrame(new Uint8Array([1]), Micro(timestamp)),
			timestamp: Time.Timestamp.fromMicros(timestamp ?? 20_000),
		});
		group.finish();
	};
	try {
		write(0n, 0);
		await flush();
		write(2n, 20_000);
		await flush();
		await clock.advance(Nano.fromMilli(Milli(50)));
		write(1n, 10_000);
		await flush();
		expect(results.map((read) => read.kind)).toEqual(["frame", "frame", "frame"]);
		write(4n);
		await flush();
		await clock.advance(Nano.fromMilli(Milli(99)));
		expect(results).toHaveLength(3);
		await clock.advance(Nano.fromMilli(Milli(1)));
		expect(results.slice(3)).toEqual([
			{ kind: "gap", timestamp: Micro(20_000), duration: Micro.zero },
			{ kind: "end", group: 4n, end: Micro(20_000) },
		]);
	} finally {
		consumer.close();
		track.finish();
		track.close();
		try {
			await reading;
		} finally {
			installed.close();
		}
	}
});

test("the holes a stalled source left behind share one budget", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("data");
	const consumer = new Consumer(track.subscribe({ delay: { max: Milli(30_000) } }), {
		format: new Format("data"),
		delay: { max: Milli(100) },
	});
	const results: Read[] = [];
	const reading = (async () => {
		for (;;) {
			const read = await consumer.next();
			if (!read) return;
			results.push(read);
		}
	})();
	const write = (sequence: bigint) => {
		const group = track.createGroup(sequence);
		group.writeFrame({
			payload: encodeFrame(new Uint8Array([1]), Micro(Number(sequence) * 10_000)),
			timestamp: Time.Timestamp.fromMicros(Number(sequence) * 10_000),
		});
		group.finish();
	};
	try {
		write(0n);
		await flush();
		for (const sequence of [2n, 4n, 6n]) write(sequence);
		await flush();
		expect(results).toHaveLength(1);
		await clock.advance(Nano.fromMilli(Milli(100)));
		expect(results.map((read) => read.kind)).toEqual(["frame", "gap", "frame", "gap", "frame", "gap", "frame"]);
	} finally {
		consumer.close();
		track.finish();
		track.close();
		try {
			await reading;
		} finally {
			installed.close();
		}
	}
});

test("a hole is late from the earliest arrival past it", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("data");
	const consumer = new Consumer(track.subscribe({ delay: { max: Milli(30_000) } }), {
		format: new Format("data"),
		delay: { max: Milli(100) },
	});
	const results: Read[] = [];
	const reading = (async () => {
		for (;;) {
			const read = await consumer.next();
			if (!read) return;
			results.push(read);
		}
	})();
	const write = (sequence: bigint) => {
		const group = track.createGroup(sequence);
		group.writeFrame({
			payload: encodeFrame(new Uint8Array([1]), Micro(Number(sequence) * 10_000)),
			timestamp: Time.Timestamp.fromMicros(Number(sequence) * 10_000),
		});
		group.finish();
	};
	try {
		write(0n);
		await flush();
		write(4n);
		await flush();
		await clock.advance(Nano.fromMilli(Milli(50)));
		write(2n);
		await flush();
		expect(results).toHaveLength(1);
		await clock.advance(Nano.fromMilli(Milli(50)));
		expect(results.map((read) => read.kind)).toEqual(["frame", "gap", "frame", "gap", "frame"]);
	} finally {
		consumer.close();
		track.finish();
		track.close();
		try {
			await reading;
		} finally {
			installed.close();
		}
	}
});

test("a hole in a drained backlog is late from the earliest arrival past it, not the first admitted", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("data");
	const marker = (sequence: bigint) => {
		const group = track.createGroup(sequence);
		group.writeFrame({
			payload: new Uint8Array(),
			timestamp: Time.Timestamp.fromMicros(Number(sequence) * 10_000),
		});
		group.finish();
	};
	const first = track.createGroup(0n);
	first.writeFrame({ payload: encodeFrame(new Uint8Array([1]), Micro(0)), timestamp: Time.Timestamp.fromMicros(0) });
	first.finish();
	marker(5n);
	await clock.advance(Nano.fromMilli(Milli(900)));
	marker(3n);
	await clock.advance(Nano.fromMilli(Milli(100)));

	// Joined now, the backlog drains in sequence order: group 3 before group 5, which landed first.
	const consumer = new Consumer(track.subscribe({ delay: { max: Milli(30_000) } }), {
		format: new Format("data"),
		delay: { max: Milli(500) },
	});
	const results: Read[] = [];
	const reading = (async () => {
		for (;;) {
			const read = await consumer.next();
			if (!read) return;
			results.push(read);
		}
	})();
	try {
		await flush();
		// Group 5 landed a second ago, so the holes before groups 3 and 5 both expired.
		expect(results.map((read) => (read.kind === "end" ? `end ${read.group}` : read.kind))).toEqual([
			"frame",
			"gap",
			"end 3",
			"gap",
			"end 5",
		]);
	} finally {
		consumer.close();
		track.finish();
		track.close();
		try {
			await reading;
		} finally {
			installed.close();
		}
	}
});

// A pause's timestamp jump is not lateness: the group that resumes it can arrive behind the one after
// it, and is waited for like any hole rather than skipped as too old.
test.each([true, false])("the head after a declared end is waited for (arrives: %p)", async (arrives) => {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("audio");
	const consumer = new Consumer(track.subscribe({ delay: { max: Milli(30_000) } }), {
		format: new Format("audio"),
		delay: { max: Milli(300) },
	});
	const results: Read[] = [];
	const reading = (async () => {
		for (;;) {
			const read = await consumer.next();
			if (!read) return;
			results.push(read);
		}
	})();
	const write = (sequence: bigint, ...frames: [number, Uint8Array?][]) => {
		const group = track.createGroup(sequence);
		for (const [timestamp, payload = new Uint8Array([1])] of frames) {
			group.writeFrame({
				payload: encodeFrame(payload, Micro(timestamp)),
				timestamp: Time.Timestamp.fromMicros(Micro(timestamp)),
			});
		}
		group.finish();
	};
	const frame = (group: bigint, timestamp: number) => ({
		kind: "frame" as const,
		group,
		frame: expect.objectContaining({ timestamp }),
	});
	try {
		write(0n, [0]);
		// The end, then the stopped encoder's final packet, as a native publisher writes them.
		write(1n, [60_000, new Uint8Array()], [40_000]);
		await flush();
		expect(results).toEqual([frame(0n, 0), { kind: "end", group: 1n, end: Micro(60_000) }, frame(1n, 40_000)]);
		write(3n, [60_060_000]);
		await flush();
		await clock.advance(Nano.fromMilli(Milli(299)));
		expect(results).toHaveLength(3);
		if (arrives) {
			write(2n, [60_000_000]);
			await flush();
			expect(results.slice(3)).toEqual([frame(2n, 60_000_000), frame(3n, 60_060_000)]);
			// Resumed, a later hole whose head starts a budget behind the newest is proven without a wait.
			write(5n, [61_000_000], [61_300_000]);
			await flush();
			expect(results.slice(5)).toEqual([
				{ kind: "gap", timestamp: Micro(60_060_000), duration: Micro(940_000) },
				frame(5n, 61_000_000),
				frame(5n, 61_300_000),
			]);
		} else {
			await clock.advance(Nano.fromMilli(Milli(1)));
			expect(results.slice(3)).toEqual([
				{ kind: "gap", timestamp: Micro(60_000), duration: Micro(60_000_000) },
				frame(3n, 60_060_000),
			]);
		}
	} finally {
		consumer.close();
		track.finish();
		track.close();
		try {
			await reading;
		} finally {
			installed.close();
		}
	}
});

// Groups go out newest first, so a flush's newest group lands just ahead of the rest. Where
// presentation left off is a budget behind it, but the missing groups end where it starts, so they
// may still play: skipping the hole would drop them on arrival, below the cursor.
test("a newest-first flush past a hole delivers the groups landing behind its newest", async () => {
	// Time stands still while the flush lands, so only the timestamps can prove the hole.
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("audio");
	const consumer = new Consumer(track.subscribe({ delay: { max: Milli(30_000) } }), {
		format: new Format("audio"),
		delay: { max: Milli(80) },
	});
	const results: Read[] = [];
	const reading = (async () => {
		for (;;) {
			const read = await consumer.next();
			if (!read) return;
			results.push(read);
		}
	})();
	// One 20ms frame per group, as a voice publisher writes them, all sent at once.
	const sent = Time.Timestamp.now();
	const write = (sequence: number) => {
		const group = track.createGroup(BigInt(sequence));
		const timestamp = Micro(sequence * 20_000);
		group.writeFrame({ payload: encodeFrame(new Uint8Array([1]), timestamp), timestamp: sent });
		group.finish();
	};
	try {
		for (const sequence of [0, 1, 2]) write(sequence);
		await flush();
		write(7);
		await flush();
		for (const sequence of [3, 4, 5, 6]) write(sequence);
		await flush();
		expect(results).toEqual(
			[0, 1, 2, 3, 4, 5, 6, 7].map((sequence) => ({
				kind: "frame",
				group: BigInt(sequence),
				frame: expect.objectContaining({ timestamp: Micro(sequence * 20_000) }),
			})),
		);
	} finally {
		consumer.close();
		track.finish();
		track.close();
		try {
			await reading;
		} finally {
			installed.close();
		}
	}
});
