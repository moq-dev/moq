import { expect, test } from "bun:test";
import { Manual, Micro, Milli, Nano, sleep } from "@moq/clock";
import { type Group, Time, Track, Varint } from "@moq/net";
import { Consumer } from "./consumer.ts";
import { Format as LegacyFormat } from "./legacy.ts";

// Standalone from consumer.test.ts so it only pulls the Legacy format, not the CMAF decoder.

/** The next frame, passing over gaps and ends. */
async function nextFrame(consumer: Consumer) {
	for (;;) {
		const next = await consumer.next();
		if (!next || next.kind === "frame") return next;
	}
}

function encodeLegacyFrame(timestamp: Micro, payload: Uint8Array): Uint8Array {
	const tsBytes = Varint.encode(timestamp);
	const data = new Uint8Array(tsBytes.byteLength + payload.byteLength);
	data.set(tsBytes, 0);
	data.set(payload, tsBytes.byteLength);
	return data;
}

/** Write one frame into an open group: `millis` on the timeline, tagged `tag`. */
function publishFrame(group: Group.Producer, millis: number, tag: number) {
	group.writeFrame({
		payload: encodeLegacyFrame(Micro.fromMilli(millis as Milli), new Uint8Array([tag])),
		timestamp: Time.Timestamp.fromMillis(millis),
	});
}

/** Publish one closed group carrying a single frame at `millis`. */
function publish(track: Track.Producer, sequence: number, millis: number) {
	const group = track.createGroup(sequence);
	publishFrame(group, millis, sequence);
	group.finish();
}

/** Every frame the consumer will hand over right now, as [group, payload] pairs. */
async function drain(consumer: Consumer): Promise<[number, number | undefined][]> {
	const seen: [number, number | undefined][] = [];
	for (;;) {
		const next = await Promise.race([consumer.next(), sleep(Nano.fromMilli(Milli(50))).then(() => undefined)]);
		if (!next) break;
		if (next.kind === "frame") seen.push([next.group, next.frame.payload[0]]);
	}
	return seen;
}

// A publisher serving a subscriber's max delay hands over the head of the window alongside
// the live edge, and groups go out newest-first, so the head arrives *after* the group
// that is already playing. Arriving in that order is not a reason to throw it away:
// audio writes into a timestamp-indexed ring and video drops a late frame at render, and the
// subscription's own max delay already bounds how far back one can be.
test("out-of-order groups are delivered rather than dropped", async () => {
	const track = new Track.Producer("test").accept({ timescale: Time.Timescale.MILLI, maxAge: Milli(30_000) });
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(5000) }), {
		format: new LegacyFormat("data"),
		maxDelay: 5000 as Milli,
	});

	// The live edge lands first, and delivery starts there rather than waiting.
	publish(track, 3, 30);
	expect((await nextFrame(consumer))?.frame.payload).toEqual(new Uint8Array([3]));

	// The window's head follows it. Every group is still within both budgets, so all three
	// are handed over even though each sits below the group already delivered.
	publish(track, 0, 0);
	publish(track, 1, 10);
	publish(track, 2, 20);

	expect(await drain(consumer)).toEqual([
		[0, 0],
		[1, 1],
		[2, 2],
	]);

	consumer.close();
});

// A below-cursor group may still be downloading when its buffered frames are momentarily
// drained (the decode loop consumes faster than the network delivers). Removing it at that
// instant silently truncates its tail, so removal must wait for the group to finish.
test("a below-cursor group still downloading is not truncated", async () => {
	const track = new Track.Producer("test").accept({ timescale: Time.Timescale.MILLI, maxAge: Milli(30_000) });
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(5000) }), {
		format: new LegacyFormat("data"),
		maxDelay: 5000 as Milli,
	});

	// The live edge group arrives first and starts delivery (still open).
	const live = track.createGroup(3);
	publishFrame(live, 3000, 30);
	expect((await nextFrame(consumer))?.frame.payload[0]).toBe(30);

	// A backlog group lands below the cursor, one frame at a time (mid-download).
	const backlog = track.createGroup(1);
	publishFrame(backlog, 1000, 10);
	const first = await nextFrame(consumer);
	expect(first?.group).toBe(1);
	expect(first?.frame.payload[0]).toBe(10);

	// The consumer finds the group's buffer empty while it is still open. It must wait
	// rather than shift the group out, so the second frame still arrives.
	const pending = nextFrame(consumer);
	publishFrame(backlog, 1500, 11);
	backlog.finish();

	const second = await pending;
	expect(second?.group).toBe(1);
	expect(second?.frame.payload[0]).toBe(11);

	live.finish();
	track.finish();
	track.close();
	consumer.close();
});

// The floor belongs to the latest group, not to delayed history within max delay.
test("delayed older groups survive a floor established by two newer groups", async () => {
	const clock = new Manual();
	clock.jump(Nano.fromMilli(Milli(200)));
	const installed = clock.install();
	const track = new Track.Producer("test").accept({ timescale: Time.Timescale.MILLI, maxAge: Milli(30_000) });
	const consumer = new Consumer(track.subscribe({ maxDelay: Milli(5000) }), {
		format: new LegacyFormat("data"),
		maxDelay: Milli(5000),
	});
	try {
		publish(track, 3, 30);
		expect((await nextFrame(consumer))?.group).toBe(3);

		const latest = track.createGroup(4);
		publishFrame(latest, 40, 4);
		expect((await nextFrame(consumer))?.group).toBe(4);

		// Group 1's 10ms frame is valid history, below group 4's 30ms floor.
		publish(track, 1, 10);
		const backlog = await nextFrame(consumer);
		expect(backlog?.group).toBe(1);
		expect(backlog?.frame.timestamp).toBe(Micro(10_000));

		// Delivering the backlog must not erase the floor for group 4 itself.
		publishFrame(latest, 29, 5);
		await expect(nextFrame(consumer)).rejects.toThrow("below the previous group start");
	} finally {
		consumer.close();
		track.finish();
		track.close();
		installed.close();
	}
});
