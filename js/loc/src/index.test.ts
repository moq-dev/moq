import { expect, test } from "bun:test";
import { Time, Track, Varint } from "@moq/net";
import { Format, Producer } from "./index.ts";

const PROP_TIMESCALE = 0x08;
const PROP_TIMESTAMP = 0x10;
const PROP_TIMESTAMP_DRAFT03 = 0x06;

function buildFrame(props: Uint8Array, payload: Uint8Array): Uint8Array {
	const lenBytes = Varint.encode(props.byteLength);
	const out = new Uint8Array(lenBytes.byteLength + props.byteLength + payload.byteLength);
	out.set(lenBytes, 0);
	out.set(props, lenBytes.byteLength);
	out.set(payload, lenBytes.byteLength + props.byteLength);
	return out;
}

function concat(...parts: Uint8Array[]): Uint8Array {
	const total = parts.reduce((n, p) => n + p.byteLength, 0);
	const out = new Uint8Array(total);
	let offset = 0;
	for (const part of parts) {
		out.set(part, offset);
		offset += part.byteLength;
	}
	return out;
}

test("Format treats an empty payload as a duration marker", () => {
	const props = concat(Varint.encode(PROP_TIMESTAMP), Varint.encode(33_000));
	const frame = buildFrame(props, new Uint8Array());
	const fmt = new Format("video");
	const [decoded] = fmt.decode(frame);
	expect(decoded.payload.byteLength).toBe(0);
	expect(fmt.end(decoded)).toBe(33_000 as Time.Micro);
});

test("Format preserves empty data payloads", () => {
	const props = concat(Varint.encode(PROP_TIMESTAMP), Varint.encode(33_000));
	const fmt = new Format();
	const [decoded] = fmt.decode(buildFrame(props, new Uint8Array()));
	expect(decoded.payload.byteLength).toBe(0);
	expect(fmt.end(decoded)).toBeUndefined();
});

test("Format decodes timestamp at default microseconds timescale", () => {
	const props = concat(Varint.encode(PROP_TIMESTAMP), Varint.encode(12_345));
	const payload = new Uint8Array([0xde, 0xad, 0xbe, 0xef]);
	const frame = buildFrame(props, payload);

	const fmt = new Format();
	const [decoded] = fmt.decode(frame);

	expect(decoded.timestamp).toBe(12_345 as Time.Micro);
	expect(decoded.payload).toEqual(payload);
	expect(decoded.keyframe).toBe(false);
});

test("Format honors per-frame timescale property", () => {
	// timestamp = 96000 at per-frame timescale 48000 -> 2 seconds = 2_000_000 micros
	const props = concat(
		Varint.encode(PROP_TIMESCALE),
		Varint.encode(48_000),
		Varint.encode(PROP_TIMESTAMP - PROP_TIMESCALE), // delta to 0x10
		Varint.encode(96_000),
	);
	const frame = buildFrame(props, new Uint8Array());

	const fmt = new Format();
	const [decoded] = fmt.decode(frame);

	expect(decoded.timestamp).toBe(2_000_000 as Time.Micro);
});

test("Format skips unknown odd-typed properties", () => {
	// 0x0d video config bytes [1,2,3], then 0x10 (delta 3) timestamp
	const props = concat(
		Varint.encode(0x0d),
		Varint.encode(3),
		new Uint8Array([0x01, 0x02, 0x03]),
		Varint.encode(PROP_TIMESTAMP - 0x0d),
		Varint.encode(10),
	);
	const payload = new Uint8Array([0xaa]);
	const frame = buildFrame(props, payload);

	const fmt = new Format();
	const [decoded] = fmt.decode(frame);

	expect(decoded.timestamp).toBe(10 as Time.Micro);
	expect(decoded.payload).toEqual(payload);
});

test("Format throws when the timestamp property is missing", () => {
	const props = concat(Varint.encode(PROP_TIMESCALE), Varint.encode(1000));
	const frame = buildFrame(props, new Uint8Array([0xff]));

	const fmt = new Format();
	expect(() => fmt.decode(frame)).toThrow(/timestamp/);
});

test("Format rejects zero per-frame timescale", () => {
	const props = concat(
		Varint.encode(PROP_TIMESCALE),
		Varint.encode(0),
		Varint.encode(PROP_TIMESTAMP - PROP_TIMESCALE),
		Varint.encode(10),
	);
	const frame = buildFrame(props, new Uint8Array([0xaa]));

	const fmt = new Format();
	expect(() => fmt.decode(frame)).toThrow(/timescale/);
});

test("Format throws when properties_length exceeds frame size", () => {
	const lenBytes = Varint.encode(100);
	const buf = new Uint8Array(lenBytes.byteLength + 1);
	buf.set(lenBytes, 0);
	buf[lenBytes.byteLength] = 0x10;

	const fmt = new Format();
	expect(() => fmt.decode(buf)).toThrow();
});

test("Format decodes the draft-03 timestamp property", () => {
	const props = concat(Varint.encode(PROP_TIMESTAMP_DRAFT03), Varint.encode(4242));
	const payload = new Uint8Array([0x01]);
	const frame = buildFrame(props, payload);

	const fmt = new Format();
	const [decoded] = fmt.decode(frame);

	expect(decoded.timestamp).toBe(4242 as Time.Micro);
	expect(decoded.payload).toEqual(payload);
});

test("Format skips an unknown property whose value needs all 62 bits", () => {
	const props = concat(
		Varint.encode(0x02),
		Varint.encode(2n ** 62n - 1n),
		Varint.encode(PROP_TIMESTAMP - 0x02),
		Varint.encode(1_000),
	);
	const [decoded] = new Format().decode(buildFrame(props, new Uint8Array([1])));
	expect(decoded.timestamp).toBe(1_000 as Time.Micro);
});

test("Format rejects a timestamp past 2^53 - 1 instead of rounding", () => {
	const props = concat(Varint.encode(PROP_TIMESTAMP), Varint.encode(2n ** 53n));
	expect(() => new Format().decode(buildFrame(props, new Uint8Array()))).toThrow(/larger than 53-bits/);
});

/** Each group's (LOC timestamp, payload size, net timestamp) after the producer has closed the track. */
async function readGroups(track: Track.Producer) {
	const subscriber = track.subscribe({ maxDelay: Time.Milli(30_000) });
	const format = new Format("video");
	const groups: { timestamp: number; size: number; net: number | undefined }[][] = [];
	for (;;) {
		const group = await subscriber.recvGroup();
		if (!group) break;
		const frames: { timestamp: number; size: number; net: number | undefined }[] = [];
		for (;;) {
			const frame = await group.readFrame();
			if (!frame) break;
			const [decoded] = format.decode(frame.payload);
			frames.push({
				timestamp: decoded.timestamp,
				size: decoded.payload.byteLength,
				net: frame.timestamp?.asMicros(),
			});
		}
		groups.push(frames);
	}
	return groups;
}

test("Producer ends a group with an empty frame at the next keyframe", async () => {
	const track = new Track.Producer("video");
	const producer = new Producer(track);
	producer.encode(new Uint8Array([0xde, 0xad]), 0 as Time.Micro, true);
	producer.encode(new Uint8Array([0xbe, 0xef]), 10_000 as Time.Micro, false);
	producer.encode(new Uint8Array([0xca, 0xfe]), 20_000 as Time.Micro, true);
	producer.close();

	const groups = await readGroups(track);
	expect(groups[0]).toEqual([
		{ timestamp: 0, size: 2, net: 0 },
		{ timestamp: 10_000, size: 2, net: 10_000 },
		{ timestamp: 20_000, size: 0, net: 20_000 },
	]);
	// The tail has no successor, so the marker is one interval after the last sample.
	expect(groups[1]).toEqual([
		{ timestamp: 20_000, size: 2, net: 20_000 },
		{ timestamp: 30_000, size: 0, net: 30_000 },
	]);
	expect(groups).toHaveLength(2);

	const marker = groups[0][2];
	expect(
		new Format("video").end({
			payload: new Uint8Array(marker.size),
			timestamp: marker.timestamp as Time.Micro,
			keyframe: false,
		}),
	).toBe(20_000 as Time.Micro);
});

test("Producer omits the marker when a group has no interval to close", async () => {
	const track = new Track.Producer("video");
	const producer = new Producer(track);
	producer.encode(new Uint8Array([1]), 0 as Time.Micro, true);
	producer.close();

	expect(await readGroups(track)).toEqual([[{ timestamp: 0, size: 1, net: 0 }]]);
});

test("Producer estimates the marker when the next keyframe overlaps an ordered tail", async () => {
	const track = new Track.Producer("video");
	const producer = new Producer(track);
	producer.encode(new Uint8Array([1]), 0 as Time.Micro, true);
	producer.encode(new Uint8Array([1]), 10_000 as Time.Micro, false);
	producer.encode(new Uint8Array([1]), 5_000 as Time.Micro, true);
	producer.close();

	expect(await readGroups(track)).toEqual([
		[
			{ timestamp: 0, size: 1, net: 0 },
			{ timestamp: 10_000, size: 1, net: 10_000 },
			{ timestamp: 20_000, size: 0, net: 20_000 },
		],
		[{ timestamp: 5_000, size: 1, net: 5_000 }],
	]);
});

test("Producer omits a reordered group's marker and a keyframe that overlaps the tail", async () => {
	const track = new Track.Producer("video");
	const producer = new Producer(track);
	for (const [index, timestamp] of [0, 120_000, 40_000, 80_000].entries()) {
		producer.encode(new Uint8Array([1]), timestamp as Time.Micro, index === 0);
	}
	// Overlaps the previous frame, so it is not that group's end, and the cadence does not carry over.
	producer.encode(new Uint8Array([1]), 50_000 as Time.Micro, true);
	producer.close();

	expect(await readGroups(track)).toEqual([
		[
			{ timestamp: 0, size: 1, net: 0 },
			{ timestamp: 120_000, size: 1, net: 120_000 },
			{ timestamp: 40_000, size: 1, net: 40_000 },
			{ timestamp: 80_000, size: 1, net: 80_000 },
		],
		[{ timestamp: 50_000, size: 1, net: 50_000 }],
	]);
});
