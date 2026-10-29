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
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		maxDelay: Milli(budget),
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
		const first = track.createGroup(0);
		first.writeFrame({ payload: encodeFrame(new Uint8Array([1]), Micro(10_000)), timestamp: Time.Timestamp.now() });
		first.finish();
		await flush();
		expect(results).toHaveLength(1);
		const marker = track.createGroup(2);
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
			{ kind: "end", group: 2, end: Micro(10_000) },
		]);
	} finally {
		consumer.close();
		track.finish();
		track.close();
		await reading;
		installed.close();
	}
});

test("a filled hole cancels its timer and the next hole gets a fresh budget", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("data");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		maxDelay: Milli(100),
	});
	const results: Read[] = [];
	const reading = (async () => {
		for (;;) {
			const read = await consumer.next();
			if (!read) return;
			results.push(read);
		}
	})();
	const write = (sequence: number, timestamp?: number) => {
		const group = track.createGroup(sequence);
		group.writeFrame({
			payload: timestamp === undefined ? new Uint8Array() : encodeFrame(new Uint8Array([1]), Micro(timestamp)),
			timestamp: Time.Timestamp.fromMicros(timestamp ?? 20_000),
		});
		group.finish();
	};
	try {
		write(0, 0);
		await flush();
		write(2, 20_000);
		await flush();
		await clock.advance(Nano.fromMilli(Milli(50)));
		write(1, 10_000);
		await flush();
		expect(results.map((read) => read.kind)).toEqual(["frame", "frame", "frame"]);
		write(4);
		await flush();
		await clock.advance(Nano.fromMilli(Milli(99)));
		expect(results).toHaveLength(3);
		await clock.advance(Nano.fromMilli(Milli(1)));
		expect(results.slice(3)).toEqual([
			{ kind: "gap", timestamp: Micro(20_000), duration: Micro.zero },
			{ kind: "end", group: 4, end: Micro(20_000) },
		]);
	} finally {
		consumer.close();
		track.finish();
		track.close();
		await reading;
		installed.close();
	}
});

test("the holes a stalled source left behind share one budget", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("data");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		maxDelay: Milli(100),
	});
	const results: Read[] = [];
	const reading = (async () => {
		for (;;) {
			const read = await consumer.next();
			if (!read) return;
			results.push(read);
		}
	})();
	const write = (sequence: number) => {
		const group = track.createGroup(sequence);
		group.writeFrame({
			payload: encodeFrame(new Uint8Array([1]), Micro(sequence * 10_000)),
			timestamp: Time.Timestamp.fromMicros(sequence * 10_000),
		});
		group.finish();
	};
	try {
		write(0);
		await flush();
		for (const sequence of [2, 4, 6]) write(sequence);
		await flush();
		expect(results).toHaveLength(1);
		await clock.advance(Nano.fromMilli(Milli(100)));
		expect(results.map((read) => read.kind)).toEqual(["frame", "gap", "frame", "gap", "frame", "gap", "frame"]);
	} finally {
		consumer.close();
		track.finish();
		track.close();
		await reading;
		installed.close();
	}
});

test("a hole is late from the earliest arrival past it", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("data");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		maxDelay: Milli(100),
	});
	const results: Read[] = [];
	const reading = (async () => {
		for (;;) {
			const read = await consumer.next();
			if (!read) return;
			results.push(read);
		}
	})();
	const write = (sequence: number) => {
		const group = track.createGroup(sequence);
		group.writeFrame({
			payload: encodeFrame(new Uint8Array([1]), Micro(sequence * 10_000)),
			timestamp: Time.Timestamp.fromMicros(sequence * 10_000),
		});
		group.finish();
	};
	try {
		write(0);
		await flush();
		write(4);
		await flush();
		await clock.advance(Nano.fromMilli(Milli(50)));
		write(2);
		await flush();
		expect(results).toHaveLength(1);
		await clock.advance(Nano.fromMilli(Milli(50)));
		expect(results.map((read) => read.kind)).toEqual(["frame", "gap", "frame", "gap", "frame"]);
	} finally {
		consumer.close();
		track.finish();
		track.close();
		await reading;
		installed.close();
	}
});

test("a hole in a drained backlog is late from the earliest arrival past it, not the first admitted", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("data");
	const marker = (sequence: number) => {
		const group = track.createGroup(sequence);
		group.writeFrame({ payload: new Uint8Array(), timestamp: Time.Timestamp.fromMicros(sequence * 10_000) });
		group.finish();
	};
	const first = track.createGroup(0);
	first.writeFrame({ payload: encodeFrame(new Uint8Array([1]), Micro(0)), timestamp: Time.Timestamp.fromMicros(0) });
	first.finish();
	marker(5);
	await clock.advance(Nano.fromMilli(Milli(900)));
	marker(3);
	await clock.advance(Nano.fromMilli(Milli(100)));

	// Joined now, the backlog drains in sequence order: group 3 before group 5, which landed first.
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		maxDelay: Milli(500),
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
		await reading;
		installed.close();
	}
});
