import { expect, test } from "bun:test";
import { Group, Time, Track, Varint } from "@moq/net";
import { Consumer } from "./consumer.ts";
import { Format as LegacyFormat } from "./legacy.ts";

// Standalone from consumer.test.ts so it only pulls the Legacy format, not the CMAF decoder.

function encodeLegacyFrame(timestamp: Time.Micro, payload: Uint8Array): Uint8Array {
	const tsBytes = Varint.encode(timestamp);
	const data = new Uint8Array(tsBytes.byteLength + payload.byteLength);
	data.set(tsBytes, 0);
	data.set(payload, tsBytes.byteLength);
	return data;
}

/** Write one frame into an open group: `millis` on the timeline, tagged `tag`. */
function publishFrame(group: Group.Producer, millis: number, tag: number) {
	group.writeFrame({
		payload: encodeLegacyFrame(Time.Micro.fromMilli(millis as Time.Milli), new Uint8Array([tag])),
		timestamp: Time.Timestamp.fromMillis(millis),
	});
}

/** Publish one closed group carrying a single frame at `millis`. */
function publish(track: Track.Producer, sequence: number, millis: number) {
	const group = new Group.Producer(sequence);
	track.writeGroup(group);
	publishFrame(group, millis, sequence);
	group.close();
}

/** Every group and frame the consumer will hand over right now, as [group, payload] pairs. */
async function drain(consumer: Consumer): Promise<[number, number | undefined][]> {
	const seen: [number, number | undefined][] = [];
	for (;;) {
		const next = await Promise.race([consumer.next(), new Promise((resolve) => setTimeout(resolve, 50))]);
		if (!next) break;
		const { group, frame } = next as { group: number; frame?: { payload: Uint8Array } };
		seen.push([group, frame?.payload[0]]);
	}
	return seen;
}

// A publisher serving a subscriber's max age hands over the head of the window alongside
// the live edge, and groups go out newest-first, so the head arrives *after* the group
// that is already playing. Arriving in that order is not a reason to throw it away:
// audio writes into a timestamp-indexed ring and video drops a late frame at render, and the
// subscription's own max age already bounds how far back one can be.
test("out-of-order groups are delivered rather than dropped", async () => {
	const track = new Track.Producer("test").accept({ maxAge: 30_000 });
	const consumer = new Consumer(track.subscribe({ maxAge: 5000 }), {
		format: new LegacyFormat(),
		latency: 5000 as Time.Milli,
	});

	// The live edge lands first, and delivery starts there rather than waiting.
	publish(track, 3, 30);
	expect((await consumer.next())?.frame?.payload).toEqual(new Uint8Array([3]));

	// The window's head follows it. Every group is still within both budgets, so all three
	// are handed over even though each sits below the group already delivered.
	publish(track, 0, 0);
	publish(track, 1, 10);
	publish(track, 2, 20);

	const delivered = (await drain(consumer)).filter(([, payload]) => payload !== undefined);
	expect(delivered).toEqual([
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
	const track = new Track.Producer("test").accept({ maxAge: 30_000 });
	const consumer = new Consumer(track.subscribe({ maxAge: 5000 }), {
		format: new LegacyFormat(),
		latency: 5000 as Time.Milli,
	});

	// The live edge group arrives first and starts delivery (still open).
	const live = new Group.Producer(3);
	track.writeGroup(live);
	publishFrame(live, 3000, 30);
	expect((await consumer.next())?.frame?.payload[0]).toBe(30);

	// A backlog group lands below the cursor, one frame at a time (mid-download).
	const backlog = new Group.Producer(1);
	track.writeGroup(backlog);
	publishFrame(backlog, 1000, 10);
	const first = await consumer.next();
	expect(first?.group).toBe(1);
	expect(first?.frame?.payload[0]).toBe(10);

	// The consumer finds the group's buffer empty while it is still open. It must wait
	// rather than shift the group out, so the second frame still arrives.
	const pending = consumer.next();
	publishFrame(backlog, 1500, 11);
	backlog.close();

	const second = (await pending) as { group: number; frame?: { payload: Uint8Array } };
	expect(second.group).toBe(1);
	expect(second.frame?.payload[0]).toBe(11);

	live.close();
	track.close();
	consumer.close();
});
