import { afterEach, beforeEach, expect, test } from "bun:test";
import { Duration, type Installed } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Error as NetError, Time, Track, Varint } from "@moq/net";
import { Format, Producer } from "./legacy.ts";

// Frozen: nothing here waits on time.
let installed: Installed;
beforeEach(() => {
	installed = new Manual().install();
});
afterEach(() => installed.close());

// A track at `scale` rather than hang's pinned microseconds, holding every group for the reader.
function track(scale: Time.Timescale): Track.Producer {
	return new Track.Producer("legacy").accept({ timescale: scale, maxAge: null });
}

// Every group the producer wrote: its start, whole end, and each frame's wire timestamp.
async function groups(subscriber: Track.Subscriber) {
	const out: { start: bigint; end?: bigint; frames: number[] }[] = [];
	for (;;) {
		const group = await subscriber.recvGroup();
		if (!group) return out;
		const frames: number[] = [];
		for (;;) {
			const frame = await group.readFrame();
			if (!frame) break;
			frames.push(Varint.decode(frame.payload)[0]);
		}
		out.push({ start: group.start, end: group.end, frames });
	}
}

test("Legacy Producer keys groups in a millisecond track's timescale", async () => {
	const media = track(Time.Timescale.MILLI);
	const subscriber = media.subscribe({ delay: { max: Duration.millis(30_000) } });
	const producer = new Producer(media, new Format("video"));
	producer.encode(new Uint8Array([1]), Duration.micros(1_400), true);
	// The same millisecond as the open group's start, so it continues that group.
	producer.encode(new Uint8Array([1]), Duration.micros(1_900), true);
	producer.encode(new Uint8Array([1]), Duration.micros(34_700), true);
	producer.encode(new Uint8Array([1]), Duration.micros(68_100), false);
	producer.discontinuity();
	producer.finish();

	expect(await groups(subscriber)).toEqual([
		// Its end marker at the next keyframe, which it ends whole at.
		{ start: 1n, end: 34n, frames: [1_400, 1_900, 34_700] },
		{ start: 34n, end: 68n, frames: [34_700, 68_100] },
		// Not below the newest frame's millisecond, so the marker starts there.
		{ start: 68n, end: undefined, frames: [68_000] },
	]);
});

test("Legacy Producer keys groups in a nanosecond track's timescale", async () => {
	const media = track(Time.Timescale.NANO);
	const subscriber = media.subscribe({ delay: { max: Duration.millis(30_000) } });
	const producer = new Producer(media, new Format("video"));
	producer.encode(new Uint8Array([1]), Duration.micros(1_000), true);
	producer.encode(new Uint8Array([1]), Duration.micros(2_000), true);
	producer.discontinuity(Duration.micros(3_000));
	producer.finish();

	expect(await groups(subscriber)).toEqual([
		{ start: 1_000_000n, end: 2_000_000n, frames: [1_000, 2_000] },
		// Its end marker at 3ms lies past the marker group one nanosecond before it, and a group
		// never ends below a frame of its own, so it closes without an end.
		{ start: 2_000_000n, end: undefined, frames: [2_000, 3_000] },
		{ start: 2_999_999n, end: undefined, frames: [2_999] },
	]);
});

test("Legacy Producer skips the keys below a non-microsecond track's first group", async () => {
	const media = track(Time.Timescale.MILLI);
	const producer = new Producer(media, new Format("video"));
	producer.encode(new Uint8Array([1]), Duration.micros(5_500), true);
	// A group at a skipped key is refused, so the skip covered every millisecond below 5.
	expect(() => media.appendGroup(Time.Timestamp.fromMillis(4))).toThrow();
	expect(media.largest()?.group).toBe(5n);
	producer.finish();
});

test("Legacy Producer refuses a scale-less track", () => {
	const media = new Track.Producer("legacy").accept({ timescale: null });
	const producer = new Producer(media, new Format("video"));
	expect(() => producer.encode(new Uint8Array([1]), Duration.micros(0), true)).toThrow(NetError.TimestampMismatch);
});
