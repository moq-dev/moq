import { afterEach, beforeEach, expect, test } from "bun:test";
import { Duration, type Installed } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { type Group, Time, Track, Varint } from "@moq/net";
import { Consumer } from "./consumer.ts";
import { Format as LegacyFormat } from "./legacy.ts";

// Standalone from consumer.test.ts so it only pulls the Legacy format, not the CMAF decoder.

// Every test runs on its own Manual clock, so the consumer's waits move only when a test advances it.
let clock: Manual;
let installed: Installed;
beforeEach(() => {
	clock = new Manual();
	installed = clock.install();
});
afterEach(() => {
	installed.close();
});

/** Flush the consumer's readers without moving time, then return what `read` settled with, or "pending". */
async function poll<T>(read: Promise<T>): Promise<T | "pending"> {
	const flushed = clock.advance(Duration.ZERO);
	try {
		return await Promise.race([read, flushed.then(() => "pending" as const)]);
	} finally {
		await flushed;
	}
}

/** The next frame, passing over gaps and ends. */
async function nextFrame(consumer: Consumer) {
	for (;;) {
		const next = await consumer.next();
		if (!next || next.kind === "frame") return next;
	}
}

function encodeLegacyFrame(timestamp: number, payload: Uint8Array): Uint8Array {
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
	// waits (and lets #checkMaxDelay / #tryDurationSkip decide once the max delay budget or duration
	// coverage proves the gap too old) rather than eagerly skipping ahead, which would drop an
	// in-transit group and wreck audio.
	//
	// A hang group can only promote early once its end is known, e.g. via an end-of-group signal such
	// as a trailing frame carrying the group's end timestamp. Until the container carries that, expect
	// the wait. The CMAF path (per-sample duration in the moof) is covered in consumer.test.ts.
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe(), {
		format: new LegacyFormat("data"),
		delay: { max: Duration.millis(5000) },
	});

	// Group A at a large sequence, completed so the cursor advances past it.
	const a = track.createGroup(1_000_000n);
	a.writeFrame({
		payload: encodeLegacyFrame(0, new Uint8Array([0x01])),
		timestamp: Time.Timestamp.now(),
	});
	a.finish();

	// Drain A's frame.
	const firstFrame = await nextFrame(consumer);
	expect(firstFrame?.frame.payload).toEqual(new Uint8Array([0x01]));

	// Park next() before the next group's frames arrive (the live streaming case).
	const pending = consumer.next();

	// Open group B at a large jump (+90_000, NOT +1) with a frame far past A's last PTS, and leave it
	// open. Legacy has no duration, so A's end is unknown -- B could be continuous, or group 1_000_001
	// could still be coming. We cannot tell, so B must NOT surface yet.
	const b = track.createGroup(1_090_000n);
	b.writeFrame({
		payload: encodeLegacyFrame(1_000_000, new Uint8Array([0x02])),
		timestamp: Time.Timestamp.now(),
	});

	// Within the max delay budget (5s), the parked next() stays parked: no duration means no proof of
	// contiguity, so the consumer waits instead of eagerly delivering across the gap.
	await clock.advance(Duration.millis(4_999));
	expect(await poll(pending)).toBe("pending");

	consumer.close();
});

// A skip past a group already begun records where it ended, as Rust's `skip_to` does, so a later group
// continuing its timeline reads on without a gap. Sparse sequence numbers leave only the timeline to
// prove nothing is missing, and the empty group in between says nothing of it.
test("Consumer reads on without a gap past a group it skipped", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe(), {
		format: new LegacyFormat("audio"),
		delay: { max: Duration.millis(500) },
	});
	const write = (group: Group.Producer, timestamps: number[]) => {
		for (const timestamp of timestamps) {
			group.writeFrame({
				payload: encodeLegacyFrame(timestamp, new Uint8Array([0x01])),
				timestamp: Time.Timestamp.now(),
			});
		}
	};

	const first = track.createGroup(0n);
	write(first, [0, 100_000, 200_000, 300_000, 400_000]);
	for (let i = 0; i < 5; i++) expect((await nextFrame(consumer))?.group).toBe(0n);
	// The first group finishes, read to its end but not yet passed, followed by an adjacent empty group
	// and a group continuing its timeline. That group's start falls past the age bound behind the newest
	// frame, so the age skip passes the first group cleanly, then the empty one, which ends without a
	// frame and leaves only the skipped group's end to show the timeline goes on.
	first.finish();
	const empty = track.createGroup(1n);
	await clock.advance(Duration.ZERO);
	const last = track.createGroup(20n);
	write(last, [400_000, 1_000_000]);
	last.finish();
	empty.finish();
	await clock.advance(Duration.ZERO);
	const next = await poll(consumer.next());
	expect(next).toMatchObject({ kind: "frame", group: 20n });

	consumer.close();
	track.finish();
	track.close();
});
