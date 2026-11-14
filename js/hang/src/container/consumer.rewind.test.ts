import { afterEach, beforeEach, expect, test } from "bun:test";
import { Duration, type Installed } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Time, Track } from "@moq/net";
import { Consumer } from "./consumer";
import { encodeFrame, Format, Producer } from "./legacy";

// Frozen at 200ms: nothing in these tests waits on time.
let installed: Installed;
beforeEach(() => {
	const clock = new Manual();
	clock.jump(Duration.millis(200));
	installed = clock.install();
});
afterEach(() => installed.close());

function setup() {
	const track = new Track.Producer("rewind");
	const consumer = new Consumer(track.subscribe({ delay: { max: Duration.millis(30_000) } }), {
		format: new Format("video"),
		delay: { max: Duration.millis(30_000) },
	});
	return { track, consumer };
}

function group(track: Track.Producer, timestamps: number[], end?: number) {
	const group = track.appendGroup();
	for (const timestamp of timestamps) {
		group.writeFrame({
			timestamp: Time.Timestamp.fromMicros(timestamp),
			payload: encodeFrame(new Uint8Array([1]), Duration.micros(timestamp)),
		});
	}
	if (end !== undefined) {
		group.writeFrame({
			timestamp: Time.Timestamp.fromMicros(end),
			payload: encodeFrame(new Uint8Array(), Duration.micros(end)),
		});
	}
	group.finish();
}

async function frame(consumer: Consumer) {
	for (;;) {
		const result = await consumer.next();
		if (!result || result.kind === "frame") return result;
	}
}

test("Consumer accepts a resuming keyframe before the estimated group end", async () => {
	const { track, consumer } = setup();
	try {
		// A cut estimates the final 33ms frame's end at 66ms. Playback consumes that
		// marker before the encoder resumes at 50ms, so the end cannot be a floor.
		group(track, [0, 33_000], 66_000);
		expect((await frame(consumer))?.frame?.timestamp.asMicros()).toBe(0);
		expect((await frame(consumer))?.frame?.timestamp.asMicros()).toBe(33_000);
		group(track, [50_000]);
		expect((await frame(consumer))?.frame?.timestamp.asMicros()).toBe(50_000);
	} finally {
		consumer.close();
		track.finish();
		track.close();
	}
});

for (const start of [16_000, 0]) {
	test(`Producer and Consumer accept the next group starting at ${start}us`, async () => {
		const { track, consumer } = setup();
		const producer = new Producer(track, new Format("video"));
		try {
			producer.encode(new Uint8Array([1]), Duration.micros(0), true);
			producer.encode(new Uint8Array([1]), Duration.micros(33_000), false);
			expect((await frame(consumer))?.group).toBe(0n);
			await frame(consumer);
			producer.encode(new Uint8Array([1]), Duration.micros(start), true);
			expect((await frame(consumer))?.group).toBe(1n);
		} finally {
			consumer.close();
			producer.finish();
		}
	});
}

test("Consumer accepts leading pictures below the previous group's content", async () => {
	const { track, consumer } = setup();
	try {
		group(track, [100_000, 180_000]);
		await frame(consumer);
		await frame(consumer);
		group(track, [200_000, 150_000]);
		expect((await frame(consumer))?.frame?.timestamp.asMicros()).toBe(200_000);
		expect((await frame(consumer))?.frame?.timestamp.asMicros()).toBe(150_000);
	} finally {
		consumer.close();
		track.finish();
		track.close();
	}
});

test("Consumer rejects a later frame below the previous group start", async () => {
	const { track, consumer } = setup();
	try {
		group(track, [100_000]);
		await frame(consumer);
		const current = track.appendGroup();
		current.writeFrame({
			timestamp: Time.Timestamp.fromMicros(200_000),
			payload: encodeFrame(new Uint8Array([1]), Duration.micros(200_000)),
		});
		await frame(consumer);
		current.writeFrame({
			timestamp: Time.Timestamp.fromMicros(99_000),
			payload: encodeFrame(new Uint8Array([1]), Duration.micros(99_000)),
		});
		await expect(frame(consumer)).rejects.toThrow("below");
	} finally {
		consumer.close();
		track.finish();
		track.close();
	}
});

test("Producer refuses a rewind before closing the current group", () => {
	const track = new Track.Producer("rewind");
	const producer = new Producer(track, new Format("video"));
	try {
		producer.encode(new Uint8Array([1]), Duration.micros(100_000), true);
		expect(() => producer.encode(new Uint8Array([1]), Duration.micros(99_000), true)).toThrow("below");
		// The refused keyframe must not close the group and strand its delta frames.
		producer.encode(new Uint8Array([1]), Duration.micros(110_000), false);
		producer.encode(new Uint8Array([1]), Duration.micros(200_000), true);
		producer.encode(new Uint8Array([1]), Duration.micros(150_000), false);
		expect(() => producer.encode(new Uint8Array([1]), Duration.micros(99_000), false)).toThrow("below");
	} finally {
		producer.finish();
	}
});
