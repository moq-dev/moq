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
	const subscriber = media.subscribe({ order: "ascending", ranges: [{}] });
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
	const subscriber = media.subscribe({ order: "ascending", ranges: [{}] });
	const producer = new Producer(media, new Format("video"));
	producer.encode(new Uint8Array([1]), Duration.micros(1_000), true);
	producer.encode(new Uint8Array([1]), Duration.micros(2_000), true);
	producer.discontinuity(Duration.micros(3_000));
	producer.finish();

	expect(await groups(subscriber)).toEqual([
		{ start: 1_000_000n, end: 2_000_000n, frames: [1_000, 2_000] },
		// A group never ends below a frame of its own, here its end marker at 3ms, so the marker
		// group starts there rather than a nanosecond before, and this group ends whole at it.
		{ start: 2_000_000n, end: 3_000_000n, frames: [2_000, 3_000] },
		{ start: 3_000_000n, end: undefined, frames: [3_000] },
	]);
});

test("Legacy Producer ends an audio group whole at its discontinuity marker", async () => {
	const media = track(Time.Timescale.MICRO);
	const subscriber = media.subscribe({ order: "ascending", ranges: [{}] });
	const producer = new Producer(media, new Format("audio"));
	producer.encode(new Uint8Array([1]), Duration.micros(0), true);
	producer.encode(new Uint8Array([1]), Duration.micros(20_000), false);
	producer.discontinuity(Duration.micros(40_000));
	// Resumed right where the content stopped.
	producer.encode(new Uint8Array([1]), Duration.micros(40_000), true);
	producer.finish();

	const published = await groups(subscriber);
	expect(published).toEqual([
		// Audio writes no end marker, so the marker group starts a unit before the end, and this
		// group ends whole there.
		{ start: 0n, end: 39_999n, frames: [0, 20_000] },
		{ start: 39_999n, end: undefined, frames: [39_999] },
		{ start: 40_000n, end: undefined, frames: [40_000] },
	]);
});

// A cut ends its group whole without a marker, and its span runs to that end: a lite-07 relay that
// has the group's End treats any start inside the span as skipped, so nothing later starts there.
test("Legacy Producer starts nothing inside the span of a group a cut ended", async () => {
	const media = track(Time.Timescale.MICRO);
	const subscriber = media.subscribe({ order: "ascending", ranges: [{}] });
	const producer = new Producer(media, new Format("audio"));
	producer.encode(new Uint8Array([1]), Duration.micros(0), true);
	producer.encode(new Uint8Array([1]), Duration.micros(20_000), false);
	producer.cut(Duration.micros(40_000));
	producer.discontinuity(Duration.micros(40_000));
	producer.encode(new Uint8Array([1]), Duration.micros(60_000), true);
	producer.encode(new Uint8Array([1]), Duration.micros(80_000), false);
	// Estimated from the cadence: 100 ms.
	producer.cut();
	producer.encode(new Uint8Array([1]), Duration.micros(90_000), true);
	producer.cut();
	producer.discontinuity();
	producer.encode(new Uint8Array([1]), Duration.micros(110_000), true);
	producer.finish();

	expect(await groups(subscriber)).toEqual([
		{ start: 0n, end: 40_000n, frames: [0, 20_000] },
		// Not a unit before the break, which the group the cut ended still spans.
		{ start: 40_000n, end: undefined, frames: [40_000] },
		{ start: 60_000n, end: 100_000n, frames: [60_000, 80_000] },
		// Resumed below the estimated end, so stamped at it.
		{ start: 100_000n, end: 120_000n, frames: [100_000] },
		// The marker starts at the estimated end, and the keyframe below it lands past both.
		{ start: 120_000n, end: undefined, frames: [120_000] },
		{ start: 120_001n, end: undefined, frames: [120_001] },
	]);
});

test("Legacy Producer starts video resuming at a discontinuity's end past its marker", async () => {
	for (const [scale, at] of [
		[Time.Timescale.MICRO, 33_001],
		[Time.Timescale.MILLI, 34_000],
	] as const) {
		const media = track(scale);
		const subscriber = media.subscribe({ order: "ascending", ranges: [{}] });
		const producer = new Producer(media, new Format("video"));
		producer.encode(new Uint8Array([1]), Duration.micros(0), true);
		producer.discontinuity(Duration.micros(33_000));
		// Resumed right where the content stopped, which the marker starts at.
		producer.encode(new Uint8Array([1]), Duration.micros(33_000), true);
		producer.finish();

		const key = (micros: number) => BigInt(Time.floorAt(Time.Timestamp.fromMicros(micros), scale));
		expect(await groups(subscriber)).toEqual([
			{ start: 0n, end: key(33_000), frames: [0, 33_000] },
			{ start: key(33_000), end: undefined, frames: [33_000] },
			// Stamped at the first start past the marker, on the wire too.
			{ start: key(at), end: undefined, frames: [at] },
		]);
	}
});

test("Legacy Producer moves a picture with the parameter sets a marker moved", async () => {
	const media = track(Time.Timescale.MICRO);
	const subscriber = media.subscribe({ order: "ascending", ranges: [{}] });
	const producer = new Producer(media, new Format("video"));
	producer.encode(new Uint8Array([1]), Duration.micros(0), true);
	producer.discontinuity(Duration.micros(33_000));
	// The parameter sets, then their picture, both at the marker.
	producer.encode(new Uint8Array([1]), Duration.micros(33_000), true);
	producer.encode(new Uint8Array([1]), Duration.micros(33_000), true);
	producer.encode(new Uint8Array([1]), Duration.micros(66_000), false);
	producer.finish();

	// Its End follows the source's 33ms cadence past the last frame, not the moved start's.
	expect((await groups(subscriber))[2]).toEqual({
		start: 33_001n,
		end: 99_000n,
		frames: [33_001, 33_001, 66_000, 99_000],
	});
});

test("Legacy Producer reorders a group whose frame is written below its moved keyframe", async () => {
	const media = track(Time.Timescale.MILLI);
	const subscriber = media.subscribe({ order: "ascending", ranges: [{}] });
	const producer = new Producer(media, new Format("video"));
	producer.encode(new Uint8Array([1]), Duration.micros(0), true);
	producer.discontinuity(Duration.micros(33_000));
	// Moved to the next millisecond past the marker, while the frame after it stays below.
	producer.encode(new Uint8Array([1]), Duration.micros(33_000), true);
	producer.encode(new Uint8Array([1]), Duration.micros(33_500), false);
	producer.finish();

	// Its presentation end no longer bounds its decode-order tail, so no end marker says so.
	expect((await groups(subscriber))[2]).toEqual({ start: 34n, end: undefined, frames: [34_000, 33_500] });
});

test("Legacy Producer refuses an end below a frame of the open group, for every kind", () => {
	for (const kind of ["audio", "video", "data"] as const) {
		const media = track(Time.Timescale.MICRO);
		const producer = new Producer(media, new Format(kind));
		producer.encode(new Uint8Array([1]), Duration.micros(0), true);
		producer.encode(new Uint8Array([1]), Duration.micros(40_000), false);
		// Reordered: the newest frame presents before the last one decoded.
		producer.encode(new Uint8Array([1]), Duration.micros(20_000), false);
		expect(() => producer.discontinuity(Duration.micros(30_000))).toThrow();
		// Refused before anything changed: the group is still open and takes the next frame.
		producer.encode(new Uint8Array([1]), Duration.micros(60_000), false);
		producer.discontinuity(Duration.micros(80_000));
		producer.finish();
	}
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
