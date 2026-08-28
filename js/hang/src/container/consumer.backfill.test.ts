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

/** Publish one closed group carrying a single frame at `millis`. */
function publish(track: Track.Producer, sequence: number, millis: number) {
	const group = new Group.Producer(sequence);
	track.writeGroup(group);
	group.writeFrame({
		payload: encodeLegacyFrame(Time.Micro.fromMilli(millis as Time.Milli), new Uint8Array([sequence])),
		timestamp: Time.Timestamp.fromMillis(millis),
	});
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

// A publisher resolving a start from the subscriber's max age serves the head of the window
// alongside the live edge, and groups go out newest-first, so the head arrives *after* the
// group that is already playing. A consumer placing frames by timestamp (the audio ring) can
// still use every one of them, so they must not be dropped for arriving in that order.
test("out-of-order groups reach a consumer that can place them", async () => {
	const track = new Track.Producer("test").accept({ maxAge: 30_000 });
	const consumer = new Consumer(track.subscribe({ maxAge: 5000 }), {
		format: new LegacyFormat(),
		latency: 5000 as Time.Milli,
		outOfOrder: true,
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
});

// The default stays strict: a decoder that consumes in order has nothing to do with a GoP
// that arrives after a newer one, so the cursor is a floor as before.
test("a late group is dropped unless the consumer opts in", async () => {
	const track = new Track.Producer("test").accept({ maxAge: 30_000 });
	const consumer = new Consumer(track.subscribe({ maxAge: 5000 }), {
		format: new LegacyFormat(),
		latency: 5000 as Time.Milli,
	});

	publish(track, 3, 30);
	expect((await consumer.next())?.frame?.payload).toEqual(new Uint8Array([3]));

	publish(track, 0, 0);
	publish(track, 1, 10);
	publish(track, 2, 20);

	const delivered = (await drain(consumer)).filter(([, payload]) => payload !== undefined);
	expect(delivered).toEqual([]);
});
