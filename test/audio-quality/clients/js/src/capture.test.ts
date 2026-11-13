import { expect, test } from "bun:test";
import { Duration, Manual, yieldNow } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import { Legacy } from "@moq/hang/container";
import { Group, Error as NetError, Origin, Path, Time } from "@moq/net";
import { Signal } from "@moq/signals";
import { Broadcast } from "../../../../../js/watch/src/broadcast.ts";
import { arrivals, Capture, format } from "./capture.ts";
import type { Arrival } from "./schema.ts";

const config = Catalog.AudioConfigSchema.parse({
	codec: "opus",
	sampleRate: 48000,
	numberOfChannels: 1,
	container: { kind: "legacy" },
});

test("arrival capture stamps a later group before a stalled earlier group", async () => {
	const clock = new Manual();
	const installed = clock.install();
	clock.jump(Duration.millis(10));
	const early = new Group.Producer(0n);
	const late = new Group.Producer(1n);
	const captured: Arrival[] = [];
	const read = async (group: Group.Consumer) => {
		for await (const frame of arrivals(group, { decoder: new Legacy.Format("audio"), config }))
			captured.push(frame);
	};
	try {
		const waiting = read(early.consume());
		late.writeFrame({
			timestamp: Time.Timestamp.fromMicros(20_000),
			payload: Legacy.encodeFrame(new Uint8Array([1]), Duration.micros(20_000)),
		});
		late.finish();
		await read(late.consume());
		expect(captured).toEqual([[10, 20, 1, 20]]);
		clock.jump(Duration.millis(40));
		early.writeFrame({
			timestamp: Time.Timestamp.fromMicros(0),
			payload: Legacy.encodeFrame(new Uint8Array([1]), Duration.micros(0)),
		});
		early.finish();
		await waiting;
		expect(captured).toEqual([
			[10, 20, 1, 20],
			[50, 0, 0, 20],
		]);
	} finally {
		early.finish();
		late.finish();
		installed.close();
	}
});

test("arrival capture excludes duration markers", async () => {
	const group = new Group.Producer(7n);
	group.writeFrame({
		timestamp: Time.Timestamp.fromMicros(100_000),
		payload: Legacy.encodeFrame(new Uint8Array(), Duration.micros(100_000)),
	});
	group.finish();
	const captured: Arrival[] = [];
	for await (const arrival of arrivals(group.consume(), { decoder: new Legacy.Format("audio"), config }))
		captured.push(arrival);
	expect(captured).toEqual([]);
});

test("arrival capture refuses an unsupported container", () => {
	expect(() => format({ container: { kind: "unknown" } } as unknown as Catalog.AudioConfig)).toThrow(
		"unsupported container",
	);
});

test("the observer shares playback demand and closes without ending playback", async () => {
	const origin = new Origin.Producer();
	const publishedRoute = origin.route(Path.from("tone.hang"), { hidden: true });
	const published = publishedRoute.broadcast;
	const track = published.createTrack("audio", { timescale: Time.Timescale.MILLI });
	publishedRoute.announce();
	const source = new Broadcast({ origin, name: Path.from("tone.hang"), follow: false, catalogFormat: "manual" });
	const capture = new Capture({
		broadcast: new Signal(source),
		track: new Signal("audio"),
		config: new Signal({
			codec: "opus",
			sampleRate: 48000,
			numberOfChannels: 1,
			container: { kind: "legacy" },
		} as Catalog.AudioConfig),
		delay: new Signal(Duration.millis(250)),
		maxDelay: new Signal(Duration.millis(250)),
	});
	try {
		while (!track.subscription.peek()) await track.subscription.changed();
		// The player's reach: the delay plus the estimator's 2 s ceiling.
		expect(track.subscription.peek()).toMatchObject({
			priority: Catalog.PRIORITY.audio,
			delay: { max: Duration.millis(2250) },
		});
		const player = published
			.consume()
			.track("audio")
			.subscribe({ priority: Catalog.PRIORITY.audio, delay: { max: Duration.millis(2250) } });
		await capture.close();
		expect(player.closed.peek()).toBeUndefined();
		player.close();
		const demand = track.demand();
		while (demand.wanted.peek() !== undefined && demand.closed.peek() === undefined) {
			await Signal.race(demand.wanted, demand.closed);
		}
		expect(capture.error()).toBeUndefined();
	} finally {
		await capture.close();
		source.close();
		origin.close();
	}
});

test("the observer asks for the decoder's delay plus its stretch and warmup, not the target", async () => {
	// A capture cut to the target would drop the frames later than it, which are what the target
	// is measured from, so a trace recorded that way can never show the player falling short.
	const origin = new Origin.Producer();
	const publishedRoute = origin.route(Path.from("tone.hang"), { hidden: true });
	const published = publishedRoute.broadcast;
	const track = published.createTrack("audio", { timescale: Time.Timescale.MILLI });
	publishedRoute.announce();
	const source = new Broadcast({ origin, name: Path.from("tone.hang"), follow: false, catalogFormat: "manual" });
	const capture = new Capture({
		broadcast: new Signal(source),
		track: new Signal("audio"),
		config: new Signal({
			codec: "opus",
			sampleRate: 48000,
			numberOfChannels: 1,
			container: { kind: "legacy" },
			warmup: 80,
		} as Catalog.AudioConfig),
		delay: new Signal(Duration.millis(100)),
		maxDelay: new Signal(Duration.millis(100)),
		stretch: new Signal(Duration.secs(2)),
	});
	try {
		while (!track.subscription.peek()) await track.subscription.changed();
		expect(track.subscription.peek()).toMatchObject({ delay: { max: Duration.millis(2180) } });
	} finally {
		await capture.close();
		source.close();
		origin.close();
	}
});

/** Capture a legacy track, then end its first group with `end` once that group's reader is in flight. */
async function captureGroupEnd(end: (group: Group.Producer) => void): Promise<string | undefined> {
	const origin = new Origin.Producer();
	const publishedRoute = origin.route(Path.from("tone.hang"), { hidden: true });
	const published = publishedRoute.broadcast;
	const track = published.createTrack("audio", { timescale: Time.Timescale.MILLI });
	publishedRoute.announce();
	const source = new Broadcast({ origin, name: Path.from("tone.hang"), follow: false, catalogFormat: "manual" });
	const capture = new Capture({
		broadcast: new Signal(source),
		track: new Signal("audio"),
		config: new Signal({
			codec: "opus",
			sampleRate: 48000,
			numberOfChannels: 1,
			container: { kind: "legacy" },
		} as Catalog.AudioConfig),
		delay: new Signal(Duration.millis(250)),
		maxDelay: new Signal(Duration.millis(250)),
	});
	try {
		while (!track.subscription.peek()) await track.subscription.changed();
		const group = track.appendGroup();
		group.writeFrame({
			timestamp: Time.Timestamp.fromMicros(0),
			payload: Legacy.encodeFrame(new Uint8Array([1]), Duration.micros(0)),
		});
		while (capture.drain().length === 0) await yieldNow();
		end(group);
		await capture.close();
		return capture.error();
	} finally {
		await capture.close();
		source.close();
		origin.close();
	}
}

test("closing the observer waits for in-flight groups and keeps their decode failures", async () => {
	const error = await captureGroupEnd((group) =>
		group.writeFrame({ timestamp: Time.Timestamp.fromMicros(20_000), payload: new Uint8Array() }),
	);
	expect(error).toBe("Error: buffer is empty");
});

test("a reset group truncates its arrivals without failing capture", async () => {
	const error = await captureGroupEnd((group) => group.abort(new NetError.TooFarBehind()));
	expect(error).toBeUndefined();
});

test("arrival durations come from Opus packets despite missing or differently sized packets", async () => {
	const group = new Group.Producer(0n);
	for (const [timestamp, toc] of [
		[0, 0x78],
		[100_000, 0x00],
		[110_000, 0x10],
	]) {
		group.writeFrame({
			timestamp: Time.Timestamp.fromMicros(timestamp),
			payload: Legacy.encodeFrame(new Uint8Array([toc, 0]), Duration.micros(timestamp)),
		});
	}
	group.finish();
	const captured: Arrival[] = [];
	for await (const arrival of arrivals(group.consume(), { decoder: new Legacy.Format("audio"), config })) {
		captured.push(arrival);
	}
	expect(captured.map(([, timestamp, , duration]) => [timestamp, duration])).toEqual([
		[0, 20],
		[100, 10],
		[110, 40],
	]);
});

test("arrival capture uses AAC's codec duration when the container leaves it implicit", async () => {
	const group = new Group.Producer(0n);
	group.writeFrame({
		timestamp: Time.Timestamp.fromMicros(0),
		payload: Legacy.encodeFrame(new Uint8Array([1]), Duration.micros(0)),
	});
	group.finish();
	const aac = Catalog.AudioConfigSchema.parse({ ...config, codec: "mp4a.40.2", sampleRate: 44100 });
	const captured: Arrival[] = [];
	for await (const arrival of arrivals(group.consume(), { decoder: new Legacy.Format("audio"), config: aac })) {
		captured.push(arrival);
	}
	// Durations hold whole nanoseconds, so the frame duration matches to the nanosecond.
	expect(captured[0][3]).toBeCloseTo((1024 * 1000) / 44100, 6);
});

test("arrival capture refuses a packet with no usable duration", async () => {
	const group = new Group.Producer(0n);
	group.writeFrame({
		timestamp: Time.Timestamp.fromMicros(0),
		payload: Legacy.encodeFrame(new Uint8Array([0x7b]), Duration.micros(0)),
	});
	group.finish();
	const reader = arrivals(group.consume(), { decoder: new Legacy.Format("audio"), config });
	await expect(reader.next()).rejects.toThrow("no encoded frame duration for opus");
});
