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

test("Consumer waits on a non-sequential hang group because it has no per-frame duration", async () => {
	// The Legacy (hang) container encodes only a timestamp per frame, no duration, so a group's
	// presentation end is unknown until the next group arrives. With non-sequential group ids we
	// therefore cannot prove the timeline is unbroken across a jump: a large id gap is
	// indistinguishable from a real missing segment that may still be in transit. So the consumer
	// waits (and lets #checkLatency / #tryDurationSkip decide once the latency budget or duration
	// coverage proves the gap too old) rather than eagerly skipping ahead, which would drop an
	// in-transit group and wreck audio.
	//
	// A hang group can only promote early once its end is known, e.g. via an end-of-group signal such
	// as a trailing frame carrying the group's end timestamp. Until the container carries that, expect
	// the wait. The CMAF path (per-sample duration in the moof) is covered in consumer.test.ts.
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe(), { format: new LegacyFormat(), latency: 5000 as Time.Milli });

	// Group A at a large sequence, completed so the cursor advances past it.
	const a = new Group.Producer(1_000_000);
	a.writeFrame({
		payload: encodeLegacyFrame(0 as Time.Micro, new Uint8Array([0x01])),
		timestamp: Time.Timestamp.now(),
	});
	a.close();
	track.writeGroup(a);

	// Drain A: its frame, then its group-done marker.
	const firstFrame = await consumer.next();
	expect(firstFrame?.frame?.payload).toEqual(new Uint8Array([0x01]));
	await consumer.next();

	// Park next() before the next group's frames arrive (the live streaming case).
	const pending = consumer.next();

	// Open group B at a large jump (+90_000, NOT +1) with a frame far past A's last PTS, and leave it
	// open. Legacy has no duration, so A's end is unknown -- B could be continuous, or group 1_000_001
	// could still be coming. We cannot tell, so B must NOT surface yet.
	const b = new Group.Producer(1_090_000);
	track.writeGroup(b);
	b.writeFrame({
		payload: encodeLegacyFrame(1_000_000 as Time.Micro, new Uint8Array([0x02])),
		timestamp: Time.Timestamp.now(),
	});

	// Within the latency budget (5s), the parked next() stays parked: no duration means no proof of
	// contiguity, so the consumer waits instead of eagerly delivering across the gap.
	const result = await Promise.race([
		pending,
		new Promise<"timeout">((resolve) => setTimeout(() => resolve("timeout"), 500)),
	]);

	expect(result).toBe("timeout");

	consumer.close();
});
