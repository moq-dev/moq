import { expect, spyOn, test } from "bun:test";
import { Elapsed, Manual, type Micro, Milli, Nano, timeout } from "@moq/clock";
import { Time, Track } from "@moq/net";
import { Consumer, type Read } from "./consumer.ts";
import { encodeFrame, Format } from "./legacy.ts";
import type { Frame } from "./types.ts";

// Counts decoded samples, so a test can tell how many the consumer holds ahead of its reads.
class Counting extends Format {
	decoded = 0;

	override decode(frame: Uint8Array): Frame[] {
		const frames = super.decode(frame);
		this.decoded += frames.length;
		return frames;
	}
}

const PAYLOAD = new Uint8Array([0xde, 0xad]);

// Let the consumer's group tasks run until they park: on a read, or on a full max buffer.
const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

function write(group: { writeFrame(frame: { payload: Uint8Array; timestamp: Time.Timestamp }): void }, ts: number) {
	group.writeFrame({ payload: encodeFrame(PAYLOAD, ts as Micro), timestamp: Time.Timestamp.now() });
}

// The most frames the consumer may hold: past the max buffer, one more wire frame. Each held frame
// costs at least its payload and its own entry.
function maxHeld(maxBuffer: number): number {
	return Math.ceil(maxBuffer / (64 + PAYLOAD.byteLength)) + 1;
}

// Read every item. With `bounded`, check after each, once decoding has caught up, that the consumer
// holds no more than the max buffer allows, which only holds while no skip drops decoded frames unread.
async function readAll(consumer: Consumer, format: Counting, bounded?: number): Promise<Read[]> {
	const reads: Read[] = [];
	let delivered = 0;
	for (;;) {
		const read = await consumer.next();
		await flush();
		if (read === undefined) return reads;
		reads.push(read);
		if (read.kind === "frame") delivered++;
		if (bounded !== undefined) expect(format.decoded - delivered).toBeLessThanOrEqual(maxHeld(bounded));
	}
}

function timestamps(reads: Read[]): number[] {
	return reads.flatMap((read) => (read.kind === "frame" ? [read.frame.timestamp] : []));
}

test("Consumer holds frames sharing a timestamp within the max buffer", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const maxBuffer = 4096;
	const format = new Counting("data");
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format,
		maxDelay: Milli(5_000),
		maxBuffer,
	});
	try {
		// Frames packed into one timestamp have no span, so the age budget never skips the stalled
		// group they queue behind. They fill the max buffer instead, which skips it once its one frame
		// is read, so nothing decoded is dropped.
		const stalled = track.createGroup(0);
		write(stalled, 0);
		const packed = track.createGroup(1);
		for (let i = 0; i < 2_000; i++) write(packed, 10_000);
		packed.finish();
		track.finish();

		const reads = await readAll(consumer, format, maxBuffer);
		const ts = timestamps(reads);
		expect(ts.length).toBe(1 + 2_000);
		expect(ts.at(0)).toBe(0);
		expect(ts.at(-1)).toBe(10_000);
		// The stalled group's tail is lost.
		expect(reads[1]?.kind).toBe("gap");
	} finally {
		consumer.close();
		track.close();
		warn.mockRestore();
	}
});

test("Consumer holds many groups within the max buffer and reaches the live edge", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const maxBuffer = 8192;
	const format = new Counting("data");
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format,
		maxDelay: Milli(100),
		maxBuffer,
	});
	try {
		const stalled = track.createGroup(0);
		write(stalled, 0);
		for (let g = 1; g <= 500; g++) {
			const group = track.createGroup(g);
			write(group, g * 1_000);
			group.finish();
		}
		track.finish();

		// The groups' overhead and first frames fill the max buffer, and the rest wait unpulled.
		await flush();
		expect(format.decoded).toBeLessThanOrEqual(maxHeld(maxBuffer));
		expect(format.decoded).toBeGreaterThan(1);

		const reads = await readAll(consumer, format);
		const ts = timestamps(reads);
		expect(ts.at(0)).toBe(0);
		expect(ts.at(-1)).toBe(500_000);
	} finally {
		consumer.close();
		track.close();
		warn.mockRestore();
	}
});

test("Consumer skips a stalled group once group overhead alone fills the max buffer", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const format = new Counting("data");
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format,
		maxDelay: Milli(5_000),
		// Two groups' overhead, so nothing past the stalled head would decode without its successor leading.
		maxBuffer: 512,
	});
	try {
		const stalled = track.createGroup(0);
		write(stalled, 0);
		const next = track.createGroup(1);
		write(next, 10_000);
		next.finish();

		expect(await consumer.next()).toMatchObject({ kind: "frame", frame: { timestamp: 0 } });
		expect(await consumer.next()).toMatchObject({ kind: "gap" });
		expect(await consumer.next()).toMatchObject({ kind: "frame", frame: { timestamp: 10_000 } });
	} finally {
		consumer.close();
		track.close();
		warn.mockRestore();
	}
});

test("Consumer reaches a readable group past a stalled successor once group overhead fills the max buffer", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const format = new Counting("data");
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format,
		maxDelay: Milli.zero,
		// Three groups' overhead: the readable group cannot decode until the stalled ones are moved past.
		maxBuffer: 768,
	});
	try {
		const stalled = track.createGroup(0);
		write(stalled, 0);
		track.createGroup(1);
		const readable = track.createGroup(2);
		write(readable, 10_000);
		write(readable, 20_000);
		readable.finish();

		expect(await consumer.next()).toMatchObject({ kind: "frame", frame: { timestamp: 0 } });
		expect(await consumer.next()).toMatchObject({ kind: "gap" });
		expect(await consumer.next()).toMatchObject({ kind: "frame", frame: { timestamp: 10_000 } });
		expect(await consumer.next()).toMatchObject({ kind: "frame", frame: { timestamp: 20_000 } });
	} finally {
		consumer.close();
		track.close();
		warn.mockRestore();
	}
});

test("Consumer admits reads completing together one at a time against the max buffer", async () => {
	const format = new Counting("data");
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format,
		maxDelay: Milli(5_000),
		maxBuffer: 4096,
	});
	try {
		// Ten open groups park on their first read with room to spare, then large frames land in all of
		// them at once.
		const groups = Array.from({ length: 10 }, (_, sequence) => track.createGroup(sequence));
		await flush();
		const large = new Uint8Array(2_000);
		for (const group of groups) {
			group.writeFrame({ payload: encodeFrame(large, 0 as Micro), timestamp: Time.Timestamp.now() });
		}
		await flush();

		// The first decode fills the max buffer, so only the head may decode past it.
		expect(format.decoded).toBeLessThanOrEqual(2);
	} finally {
		consumer.close();
		track.close();
	}
});

// Keeps one byte of each decoded payload, a view of a wire frame mostly made of header, as LOC's is
// behind a large property block.
class Header extends Format {
	override decode(frame: Uint8Array): Frame[] {
		return super.decode(frame).map((decoded) => ({ ...decoded, payload: decoded.payload.subarray(0, 1) }));
	}
}

test("Consumer charges a small view of a large wire frame the wire frame", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Header("data"),
		maxDelay: Milli(5_000),
		maxBuffer: 16 * 1024,
	});
	try {
		const stalled = track.createGroup(0);
		write(stalled, 0);
		const padded = track.createGroup(1);
		const padding = new Uint8Array(60_000);
		for (let i = 0; i < 8; i++) {
			padded.writeFrame({
				payload: encodeFrame(padding, (10_000 + i) as Micro),
				timestamp: Time.Timestamp.now(),
			});
		}
		padded.finish();

		// The padded frames fill the max buffer behind the stalled group, which moves the cursor on to them.
		// Charging only the one byte views, the buffer never fills and the reader waits on the stall.
		expect(await within(consumer.next())).toMatchObject({ kind: "frame", frame: { timestamp: 0 } });
		expect(await within(consumer.next())).toMatchObject({ kind: "gap" });
		for (let i = 0; i < 8; i++) {
			expect(await within(consumer.next())).toMatchObject({ kind: "frame", frame: { timestamp: 10_000 + i } });
		}
	} finally {
		consumer.close();
		track.close();
		warn.mockRestore();
	}
});

test("Consumer stamps frames the max buffer held back with when they landed, not when they decoded", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		maxDelay: Milli(5_000),
		// One group's overhead and one frame: the second frame waits in the transport until the first is read.
		maxBuffer: 300,
	});
	try {
		const group = track.createGroup(0);
		write(group, 0);
		await clock.advance(Nano.fromMilli(Milli(250)));
		write(group, 10_000);
		group.finish();
		track.finish();
		// The reader is slow: nothing is read until well after both frames landed.
		await clock.advance(Nano.fromMilli(Milli(750)));

		expect(await consumer.next()).toMatchObject({ kind: "frame", frame: { timestamp: 0, arrival: 0 } });
		expect(await consumer.next()).toMatchObject({ kind: "frame", frame: { timestamp: 10_000, arrival: 250 } });
	} finally {
		consumer.close();
		track.close();
		installed.close();
	}
});

test("Consumer expires a hole behind a full max buffer from when the group past it landed", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		maxDelay: Milli(500),
		// Two groups' overhead: group 3 waits in the transport until groups 0 and 1 are read.
		maxBuffer: 512,
	});
	try {
		for (const sequence of [0, 1]) {
			const group = track.createGroup(sequence);
			write(group, sequence * 10_000);
			group.finish();
		}
		const marker = track.createGroup(3);
		marker.writeFrame({ payload: new Uint8Array(), timestamp: Time.Timestamp.now() });
		marker.finish();
		// The reader is slow: the hole before group 3 expired before anything is read.
		await clock.advance(Nano.fromMilli(Milli(1_000)));

		expect(await within(consumer.next())).toMatchObject({ kind: "frame", group: 0 });
		expect(await within(consumer.next())).toMatchObject({ kind: "frame", group: 1 });
		expect(await within(consumer.next())).toMatchObject({ kind: "gap" });
		expect(await within(consumer.next())).toMatchObject({ kind: "end", group: 3 });
	} finally {
		consumer.close();
		track.close();
		installed.close();
	}
});

for (const maxBuffer of [0, 256]) {
	test(`Consumer moves past a stalled group with a ${maxBuffer} byte max buffer`, async () => {
		const warn = spyOn(console, "warn").mockImplementation(() => {});
		const track = new Track.Producer("test");
		const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
			format: new Format("data"),
			maxDelay: Milli(5_000),
			// No more than one group's overhead: the successor is pulled anyway, so the head can be skipped.
			maxBuffer,
		});
		try {
			const stalled = track.createGroup(0);
			write(stalled, 0);
			const next = track.createGroup(1);
			write(next, 10_000);
			next.finish();

			expect(await consumer.next()).toMatchObject({ kind: "frame", frame: { timestamp: 0 } });
			expect(await consumer.next()).toMatchObject({ kind: "gap" });
			expect(await consumer.next()).toMatchObject({ kind: "frame", frame: { timestamp: 10_000 } });
		} finally {
			consumer.close();
			track.close();
			warn.mockRestore();
		}
	});
}

test("Consumer moves past a stalled backlog group below the cursor once the max buffer is full", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		maxDelay: Milli(5_000),
		// Two groups' overhead: the backlog group and the active one leave no room for the newer group.
		maxBuffer: 512,
	});
	try {
		const active = track.createGroup(3);
		write(active, 30_000);
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 3 });

		track.createGroup(1);
		const newer = track.createGroup(4);
		write(newer, 40_000);
		newer.finish();

		expect(await consumer.next()).toMatchObject({ kind: "gap" });
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 4, frame: { timestamp: 40_000 } });
	} finally {
		consumer.close();
		track.close();
		warn.mockRestore();
	}
});

test("Consumer reads what a promoted head already holds before skipping it under pressure", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		maxDelay: Milli(5_000),
		maxBuffer: 1024,
	});
	try {
		const stalled = track.createGroup(0);
		write(stalled, 0);
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 0 });

		// A later group's large frame fills the max buffer, then a frame lands in the group between,
		// stamped on the wire before the later group's.
		const between = track.createGroup(1);
		const later = track.createGroup(2);
		const early = Time.Timestamp.now();
		later.writeFrame({
			payload: encodeFrame(new Uint8Array(2_000), 20_000 as Micro),
			timestamp: Time.Timestamp.now(),
		});
		await flush();
		between.writeFrame({ payload: encodeFrame(PAYLOAD, 10_000 as Micro), timestamp: early });

		// Group 0 is skipped, but group 1 is read once promoted rather than skipped before its task runs.
		expect(await consumer.next()).toMatchObject({ kind: "gap" });
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 1, frame: { timestamp: 10_000 } });
	} finally {
		consumer.close();
		track.close();
		warn.mockRestore();
	}
});

// Settle `promise` within a second, so a reader that hangs fails the test instead of timing it out.
async function within<T>(promise: Promise<T>): Promise<T> {
	try {
		return await timeout(Nano.fromMilli(Milli(1_000)), promise);
	} catch (error) {
		if (error instanceof Elapsed) throw new Error("the reader hung");
		throw error;
	}
}

test("Consumer raises a malformed frame in a head promoted under pressure", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		maxDelay: Milli(5_000),
		maxBuffer: 1024,
	});
	try {
		const stalled = track.createGroup(0);
		write(stalled, 0);
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 0 });

		// A later group's large frame fills the max buffer, then a malformed frame lands in the open group between.
		const between = track.createGroup(1);
		const later = track.createGroup(2);
		const early = Time.Timestamp.now();
		later.writeFrame({
			payload: encodeFrame(new Uint8Array(2_000), 20_000 as Micro),
			timestamp: Time.Timestamp.now(),
		});
		await flush();
		between.writeFrame({ payload: new Uint8Array([0x80]), timestamp: early });

		// Group 0 is skipped, and group 1's decode error ends the reader rather than leaving it waiting.
		await expect(within(consumer.next())).rejects.toThrow("buffer too short");
	} finally {
		consumer.close();
		track.close();
		warn.mockRestore();
	}
});

test("Consumer raises a malformed group an age skip passes after the track finished", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		maxDelay: Milli(500),
	});
	try {
		const first = track.createGroup(0);
		write(first, 0);
		first.finish();
		const malformed = track.createGroup(1);
		const later = track.createGroup(2);
		track.finish();
		expect(await within(consumer.next())).toMatchObject({ kind: "frame", group: 0 });
		await flush();

		// Group 1 holds a valid frame then a malformed one, and group 2 starts past the max delay, so the
		// next read's decode skips group 1 for its age.
		write(malformed, 10_000);
		malformed.writeFrame({ payload: new Uint8Array([0x80]), timestamp: Time.Timestamp.now() });
		write(later, 2_000_000);
		await expect(within(consumer.next())).rejects.toThrow("buffer too short");
	} finally {
		consumer.close();
		track.close();
		warn.mockRestore();
	}
});

test("Consumer reads a finished marker group's end before skipping under pressure", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		maxDelay: Milli(5_000),
		maxBuffer: 1024,
	});
	try {
		const stalled = track.createGroup(0);
		write(stalled, 0);
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 0 });

		// A later group's large frame fills the max buffer, then the group between finishes as a data marker.
		const between = track.createGroup(1);
		const later = track.createGroup(2);
		const early = Time.Timestamp.now();
		later.writeFrame({
			payload: encodeFrame(new Uint8Array(2_000), 20_000 as Micro),
			timestamp: Time.Timestamp.now(),
		});
		await flush();
		between.writeFrame({ payload: new Uint8Array(), timestamp: early });
		between.finish();

		// Group 0 is skipped, and group 1 declares its end before group 2 is read.
		expect(await within(consumer.next())).toMatchObject({ kind: "gap" });
		expect(await within(consumer.next())).toMatchObject({ kind: "end", group: 1 });
		expect(await within(consumer.next())).toMatchObject({ kind: "frame", group: 2, frame: { timestamp: 20_000 } });
	} finally {
		consumer.close();
		track.close();
		warn.mockRestore();
	}
});

test("Consumer reads a group whose frame fails to read as cut short while the reader waits", async () => {
	const warn = spyOn(console, "warn").mockImplementation(() => {});
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(30_000) }), {
		format: new Format("data"),
		maxDelay: Milli(5_000),
	});
	try {
		const stalled = track.createGroup(0);
		write(stalled, 0);
		expect(await consumer.next()).toMatchObject({ kind: "frame", group: 0 });
		const next = track.createGroup(1);
		write(next, 10_000);
		next.finish();

		// The reader parks on group 0, then a frame lands whose payload fails once read, as a compressed
		// slice that will not inflate does. The read that takes it fails the group.
		const pending = within(consumer.next());
		await flush();
		const payload = encodeFrame(PAYLOAD, 5_000 as Micro);
		let armed = false;
		stalled.writeFrame({
			get payload() {
				if (armed) throw new Error("the slice will not inflate");
				return payload;
			},
			timestamp: Time.Timestamp.now(),
		});
		armed = true;

		expect(await pending).toMatchObject({ kind: "gap" });
		expect(await within(consumer.next())).toMatchObject({ kind: "frame", group: 1, frame: { timestamp: 10_000 } });
	} finally {
		consumer.close();
		track.close();
		warn.mockRestore();
	}
});
