import { afterEach, beforeEach, expect, spyOn, test } from "bun:test";
import { Time, Track } from "@moq/net";
import { Consumer } from "./consumer";
import { encodeFrame, Format, Producer } from "./legacy";

let clock: ReturnType<typeof spyOn>;
beforeEach(() => {
	clock = spyOn(performance, "now").mockReturnValue(200);
});
afterEach(() => clock.mockRestore());

function setup() {
	const track = new Track.Producer("rewind");
	const consumer = new Consumer(track.subscribe({ maxAge: Time.Milli(30_000) }), {
		format: new Format("video"),
		maxAge: Time.Milli(30_000),
	});
	return { track, consumer };
}

function group(track: Track.Producer, timestamps: number[], end?: number) {
	const group = track.appendGroup();
	for (const timestamp of timestamps) {
		group.writeFrame({
			timestamp: Time.Timestamp.fromMicros(Time.Micro(timestamp)),
			payload: encodeFrame(new Uint8Array([1]), Time.Micro(timestamp)),
		});
	}
	if (end !== undefined) {
		group.writeFrame({
			timestamp: Time.Timestamp.fromMicros(Time.Micro(end)),
			payload: encodeFrame(new Uint8Array(), Time.Micro(end)),
		});
	}
	group.close();
}

async function frame(consumer: Consumer) {
	for (;;) {
		const result = await consumer.next();
		if (!result || result.frame) return result;
	}
}

test("Consumer accepts a resuming keyframe before the estimated group end", async () => {
	const { track, consumer } = setup();
	try {
		// A cut estimates the final 33ms frame's end at 66ms. Playback consumes that
		// marker before the encoder resumes at 50ms, so the end cannot be a floor.
		group(track, [0, 33_000], 66_000);
		expect((await frame(consumer))?.frame?.timestamp).toBe(Time.Micro(0));
		expect((await frame(consumer))?.frame?.timestamp).toBe(Time.Micro(33_000));
		expect((await consumer.next())?.end).toBe(Time.Micro(66_000));
		group(track, [50_000]);
		expect((await frame(consumer))?.frame?.timestamp).toBe(Time.Micro(50_000));
	} finally {
		consumer.close();
		track.close();
	}
});

for (const start of [16_000, 0]) {
	test(`Producer and Consumer accept the next group starting at ${start}us`, async () => {
		const { track, consumer } = setup();
		const producer = new Producer(track, new Format("video"));
		try {
			producer.encode(new Uint8Array([1]), Time.Micro(0), true);
			producer.encode(new Uint8Array([1]), Time.Micro(33_000), false);
			expect((await frame(consumer))?.group).toBe(0);
			await frame(consumer);
			producer.encode(new Uint8Array([1]), Time.Micro(start), true);
			expect((await frame(consumer))?.group).toBe(1);
		} finally {
			consumer.close();
			producer.close();
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
		expect((await frame(consumer))?.frame?.timestamp).toBe(Time.Micro(200_000));
		expect((await frame(consumer))?.frame?.timestamp).toBe(Time.Micro(150_000));
	} finally {
		consumer.close();
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
			timestamp: Time.Timestamp.fromMicros(Time.Micro(200_000)),
			payload: encodeFrame(new Uint8Array([1]), Time.Micro(200_000)),
		});
		await frame(consumer);
		current.writeFrame({
			timestamp: Time.Timestamp.fromMicros(Time.Micro(99_000)),
			payload: encodeFrame(new Uint8Array([1]), Time.Micro(99_000)),
		});
		await expect(frame(consumer)).rejects.toThrow("below");
	} finally {
		consumer.close();
		track.close();
	}
});

test("Producer refuses a rewind before closing the current group", () => {
	const track = new Track.Producer("rewind");
	const producer = new Producer(track, new Format("video"));
	try {
		producer.encode(new Uint8Array([1]), Time.Micro(100_000), true);
		expect(() => producer.encode(new Uint8Array([1]), Time.Micro(99_000), true)).toThrow("below");
		// The refused keyframe must not close the group and strand its delta frames.
		producer.encode(new Uint8Array([1]), Time.Micro(110_000), false);
		producer.encode(new Uint8Array([1]), Time.Micro(200_000), true);
		producer.encode(new Uint8Array([1]), Time.Micro(150_000), false);
		expect(() => producer.encode(new Uint8Array([1]), Time.Micro(99_000), false)).toThrow("below");
	} finally {
		producer.close();
	}
});
