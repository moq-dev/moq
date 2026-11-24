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

test("raw rejects untimed tracks and handles empty markers", () => {
	const video = new Raw.Format("video");
	expect(() => video.decode(new Uint8Array([1]), Time.Timestamp.unscaled(0n))).toThrow(Moq.Error.TimestampMismatch);

	const ts = Time.Timestamp.fromMillis(500);
	const [marker] = video.decode(new Uint8Array(), ts);
	expect(video.end(marker)).toEqual(Duration.millis(500));

	const data = new Raw.Format("data");
	expect(data.decode(new Uint8Array(), ts)).toEqual([
		{ payload: new Uint8Array(), timestamp: Duration.millis(500), keyframe: false, duration: undefined },
	]);
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
	expect(() => video.decode(ctsFrame(0, [1]), Time.Timestamp.unscaled(0n))).toThrow(Moq.Error.TimestampMismatch);
	expect(() => video.decode(new Uint8Array(), Time.Timestamp.fromMillis(100))).toThrow();

	const ts = Time.Timestamp.fromMillis(500);
	const [marker] = video.decode(ctsFrame(33, []), ts);
	expect(video.end(marker)).toEqual(Duration.millis(533));

	const data = new Cts.Format("data");
	expect(data.decode(new Uint8Array(), ts)).toEqual([]);
});
