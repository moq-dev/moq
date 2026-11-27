import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { u53 } from "../catalog/integers.ts";
import * as Cts from "./cts.ts";
import * as Raw from "./raw.ts";

function ctsFrame(cts: number, body: number[]): Uint8Array {
	const prefix = Moq.Varint.encode(cts);
	const out = new Uint8Array(prefix.byteLength + body.length);
	out.set(prefix, 0);
	out.set(body, prefix.byteLength);
	return out;
}

test("raw decodes payload with transport timestamp and audio packet duration", () => {
	const format = new Raw.Format({
		codec: "opus",
		sampleRate: u53(48_000),
		numberOfChannels: u53(2),
		container: { kind: "raw" },
	});
	// CELT FB 20 ms single-frame Opus TOC byte.
	const opus = new Uint8Array([31 << 3, 1, 2]);
	const ts = new Time.Timestamp(90_000, Time.Timescale(90_000));
	const [frame] = format.decode(opus, ts);
	expect(frame.payload).toEqual(opus);
	expect(frame.timestamp).toEqual(Duration.secs(1));
	expect(frame.duration).toEqual(Duration.millis(20));
});

test("raw times AAC packets at the AudioSpecificConfig's core rate", () => {
	const format = (description: string) =>
		new Raw.Format({
			codec: "mp4a.40.2",
			sampleRate: u53(96_000),
			numberOfChannels: u53(2),
			description,
			container: { kind: "raw" },
		});
	const ts = new Time.Timestamp(0, Time.Timescale(90_000));
	const core48k = Duration.micros((1024 * 1e6) / 48_000);
	// A 48 kHz AAC-LC core with SBR at 96 kHz still codes 1024 samples at 48 kHz.
	expect(format("119056e580").decode(new Uint8Array([1]), ts)[0].duration).toEqual(core48k);
	// SBR at the core's own rate, and plain AAC-LC, likewise.
	expect(format("119056e598").decode(new Uint8Array([1]), ts)[0].duration).toEqual(core48k);
	expect(format("1190").decode(new Uint8Array([1]), ts)[0].duration).toEqual(core48k);
	// frameLengthFlag set: 960 samples, 20 ms at 48 kHz.
	expect(format("1194").decode(new Uint8Array([1]), ts)[0].duration).toEqual(Duration.millis(20));
});

test("raw rejects untimed tracks and handles empty markers", () => {
	const video = new Raw.Format("video");
	expect(() => video.decode(new Uint8Array([1]), Time.Timestamp.unscaled(0n))).toThrow("requires a timed track");

	const ts = Time.Timestamp.fromMillis(500);
	const [marker] = video.decode(new Uint8Array(), ts);
	expect(video.end(marker)).toEqual(Duration.millis(500));

	const data = new Raw.Format("data");
	expect(data.decode(new Uint8Array(), ts)).toEqual([]);
});

test("cts adds CTS varint in track timescale to transport timestamp", () => {
	const format = new Cts.Format("video");
	const scale = Time.Timescale(90_000);
	// DTS = 3000 (33.333ms), CTS = 6000 (66.667ms) -> PTS = 9000 (100ms).
	const [frame] = format.decode(ctsFrame(6_000, [9, 8]), new Time.Timestamp(3_000, scale));
	expect(frame.payload).toEqual(new Uint8Array([9, 8]));
	expect(frame.timestamp).toEqual(Duration.millis(100));
});

test("cts rejects untimed tracks and empty media objects, handles empty markers", () => {
	const video = new Cts.Format("video");
	expect(() => video.decode(ctsFrame(0, [1]), Time.Timestamp.unscaled(0n))).toThrow("requires a timed track");
	expect(() => video.decode(new Uint8Array(), Time.Timestamp.fromMillis(100))).toThrow();

	const ts = Time.Timestamp.fromMillis(500);
	const [marker] = video.decode(ctsFrame(33, []), ts);
	expect(video.end(marker)).toEqual(Duration.millis(533));

	const data = new Cts.Format("data");
	expect(data.decode(new Uint8Array(), ts)).toEqual([]);
});
