import { expect, spyOn, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import { Legacy } from "@moq/hang/container";
import { Group, Error as NetError, Origin, Path, Time } from "@moq/net";
import { Signal } from "@moq/signals";
import { Broadcast } from "../../../../../js/watch/src/broadcast.ts";
import { arrivals, Capture, format } from "./capture.ts";
import type { Arrival } from "./schema.ts";

test("arrival capture stamps a later group before a stalled earlier group", async () => {
	let now = 10;
	const clock = spyOn(performance, "now").mockImplementation(() => now);
	const early = new Group.Producer(0);
	const late = new Group.Producer(1);
	const captured: Arrival[] = [];
	const read = async (group: Group.Consumer) => {
		for await (const frame of arrivals(group, new Legacy.Format("audio"))) captured.push(frame);
	};
	try {
		const waiting = read(early.consume());
		late.writeFrame({
			timestamp: Time.Timestamp.fromMicros(20_000),
			payload: Legacy.encodeFrame(new Uint8Array([1]), Time.Micro(20_000)),
		});
		late.close();
		await read(late.consume());
		expect(captured).toEqual([[10, 20, 1]]);
		now = 50;
		early.writeFrame({
			timestamp: Time.Timestamp.fromMicros(0),
			payload: Legacy.encodeFrame(new Uint8Array([1]), Time.Micro(0)),
		});
		early.close();
		await waiting;
		expect(captured).toEqual([
			[10, 20, 1],
			[50, 0, 0],
		]);
	} finally {
		early.close();
		late.close();
		clock.mockRestore();
	}
});

test("arrival capture excludes duration markers", async () => {
	const group = new Group.Producer(7);
	group.writeFrame({
		timestamp: Time.Timestamp.fromMicros(100_000),
		payload: Legacy.encodeFrame(new Uint8Array(), Time.Micro(100_000)),
	});
	group.close();
	const captured: Arrival[] = [];
	for await (const arrival of arrivals(group.consume(), new Legacy.Format("audio"))) captured.push(arrival);
	expect(captured).toEqual([]);
});

test("arrival capture refuses an unsupported container", () => {
	expect(() => format({ container: { kind: "unknown" } } as unknown as Catalog.AudioConfig)).toThrow(
		"unsupported container",
	);
});

test("the observer shares playback demand and closes without ending playback", async () => {
	const origin = new Origin.Producer();
	const published = origin.createBroadcast(Path.from("tone.hang"));
	const track = published.createTrack("audio");
	published.announce();
	const source = new Broadcast({ origin, name: Path.from("tone.hang"), announced: false, catalogFormat: "manual" });
	const capture = new Capture({
		broadcast: new Signal(source),
		track: new Signal("audio"),
		config: new Signal({
			codec: "opus",
			sampleRate: 48000,
			numberOfChannels: 1,
			container: { kind: "legacy" },
		} as Catalog.AudioConfig),
		maxDelay: new Signal(Time.Milli(250)),
	});
	try {
		while (!track.subscription.peek()) await track.subscription.changed();
		expect(track.subscription.peek()).toMatchObject({ priority: Catalog.PRIORITY.audio, maxDelay: 250 });
		const player = published
			.consume()
			.track("audio")
			.subscribe({ priority: Catalog.PRIORITY.audio, maxDelay: Time.Milli(250) });
		await capture.close();
		expect(player.closed.peek()).toBeUndefined();
		player.close();
		await track.demand().unused();
		expect(capture.error()).toBeUndefined();
	} finally {
		await capture.close();
		source.close();
		origin.close();
	}
});

/** Capture a legacy track, then end its first group with `end` once that group's reader is in flight. */
async function captureGroupEnd(end: (group: Group.Producer) => void): Promise<string | undefined> {
	const origin = new Origin.Producer();
	const published = origin.createBroadcast(Path.from("tone.hang"));
	const track = published.createTrack("audio");
	published.announce();
	const source = new Broadcast({ origin, name: Path.from("tone.hang"), announced: false, catalogFormat: "manual" });
	const capture = new Capture({
		broadcast: new Signal(source),
		track: new Signal("audio"),
		config: new Signal({
			codec: "opus",
			sampleRate: 48000,
			numberOfChannels: 1,
			container: { kind: "legacy" },
		} as Catalog.AudioConfig),
		maxDelay: new Signal(Time.Milli(250)),
	});
	try {
		while (!track.subscription.peek()) await track.subscription.changed();
		const group = track.appendGroup();
		group.writeFrame({
			timestamp: Time.Timestamp.fromMicros(0),
			payload: Legacy.encodeFrame(new Uint8Array([1]), Time.Micro(0)),
		});
		while (capture.drain().length === 0) await new Promise((resolve) => setTimeout(resolve, 0));
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
	const error = await captureGroupEnd((group) => group.close(new NetError.TooFarBehind()));
	expect(error).toBeUndefined();
});
