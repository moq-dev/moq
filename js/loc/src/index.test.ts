import { expect, test } from "bun:test";
import type { Micro } from "@moq/clock";
import { Time, Varint } from "@moq/net";
import { Format, type Frame as LocFrame, ObjectFormat } from "./index.ts";

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
	expect(fmt.end(decoded)).toBe(33_000 as Micro);
});

test("Format preserves empty data payloads", () => {
	const props = concat(Varint.encode(PROP_TIMESTAMP), Varint.encode(33_000));
	const fmt = new Format();
	const [decoded] = fmt.decode(buildFrame(props, new Uint8Array()));
	expect(decoded.payload.byteLength).toBe(0);
	expect(fmt.end(decoded)).toBeUndefined();
});

test("Format decodes no frame from a data track's empty object and throws on a media track's", () => {
	expect(new Format("data").decode(new Uint8Array(0))).toEqual([]);
	expect(() => new Format("video").decode(new Uint8Array(0))).toThrow();
});

test("Format decodes timestamp at default microseconds timescale", () => {
	const props = concat(Varint.encode(PROP_TIMESTAMP), Varint.encode(12_345));
	const payload = new Uint8Array([0xde, 0xad, 0xbe, 0xef]);
	const frame = buildFrame(props, payload);

	const fmt = new Format();
	const [decoded] = fmt.decode(frame);

	expect(decoded.timestamp).toBe(12_345 as Micro);
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

	expect(decoded.timestamp).toBe(2_000_000 as Micro);
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

	expect(decoded.timestamp).toBe(10 as Micro);
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

	expect(decoded.timestamp).toBe(4242 as Micro);
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
	expect(decoded.timestamp).toBe(1_000 as Micro);
});

test("Format rejects a timestamp past 2^53 - 1 instead of rounding", () => {
	const props = concat(Varint.encode(PROP_TIMESTAMP), Varint.encode(2n ** 53n));
	expect(() => new Format().decode(buildFrame(props, new Uint8Array()))).toThrow(/larger than 53-bits/);
});

// draft-ietf-moq-loc-04 framing: the public properties arrive as the MoQ Object Properties, which
// js/net hands over by id, and the payload is the bare codec bitstream.

const AVC_IDR = new Uint8Array([0x00, 0x00, 0x00, 0x05, 0x65, 0x88, 0x84, 0x00, 0x33]);
const AVCC = new Uint8Array([0x01, 0x42, 0xe0, 0x1e, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x42, 0xe0, 0x1e]);
const AAC_FRAME = new Uint8Array([0x21, 0x10, 0x04, 0x60, 0x8c, 0x1c]);
const AAC_ASC = new Uint8Array([0x11, 0x90]);

function props(entries: [number, bigint | Uint8Array][]): Map<bigint, bigint | Uint8Array> {
	return new Map(entries.map(([id, value]) => [BigInt(id), value]));
}

test("ObjectFormat decodes a bare AVC payload with its video properties", () => {
	const fmt = new ObjectFormat("video");
	const [frame] = fmt.decode(
		AVC_IDR,
		undefined,
		props([
			[0x08, 15360n],
			[0x09, new Uint8Array([0xa0])],
			[0x0d, AVCC],
			[0x10, 15360n * 3n + 512n],
		]),
	);
	expect(frame.payload).toBe(AVC_IDR);
	expect(frame.timestamp).toBe(3_033_333 as Micro);
	expect(frame.keyframe).toBe(true);
	expect(frame.config).toBe(AVCC);
});

test("ObjectFormat decodes a bare AAC payload with its audio properties", () => {
	const [frame] = new ObjectFormat("audio").decode(
		AAC_FRAME,
		undefined,
		props([
			[0x08, 48000n],
			[0x0d, AVCC],
			[0x0f, AAC_ASC],
			[0x10, 96_000n],
		]),
	);
	expect(frame.payload).toBe(AAC_FRAME);
	expect(frame.timestamp).toBe(2_000_000 as Micro);
	// Audio takes its own config, never a video one.
	expect(frame.config).toBe(AAC_ASC);
});

test("ObjectFormat reads a Timestamp without a Timescale as microseconds", () => {
	const [frame] = new ObjectFormat("video").decode(AVC_IDR, undefined, props([[0x10, 1_700_000_000_123_456n]]));
	expect(frame.timestamp).toBe(1_700_000_000_123_456 as Micro);
	// No Frame Marking: only the group start is a keyframe, which the consumer decides.
	expect(frame.keyframe).toBe(false);
	// No Video Config: the catalog's description stands.
	expect(frame.config).toBeUndefined();
});

test("ObjectFormat takes the track's TIMESCALE when the object states none", () => {
	const [frame] = new ObjectFormat("video").decode(
		AVC_IDR,
		new Time.Timestamp(30, Time.Timescale(15)),
		props([[0x10, 30n]]),
	);
	expect(frame.timestamp).toBe(2_000_000 as Micro);
});

test("ObjectFormat's object Timescale outranks the track's", () => {
	const [frame] = new ObjectFormat("video").decode(
		AVC_IDR,
		new Time.Timestamp(30, Time.Timescale(15)),
		props([
			[0x08, 10n],
			[0x10, 30n],
		]),
	);
	expect(frame.timestamp).toBe(3_000_000 as Micro);
});

test("ObjectFormat marks a delta frame from its Frame Marking", () => {
	const [frame] = new ObjectFormat("video").decode(
		AVC_IDR,
		undefined,
		props([
			[0x09, new Uint8Array([0x80])],
			[0x10, 1n],
		]),
	);
	expect(frame.keyframe).toBe(false);
});

test("ObjectFormat refuses an object without Object Properties instead of reading the payload", () => {
	// An in-payload LOC frame, as moq-lite would deliver it.
	const inline = buildFrame(concat(Varint.encode(PROP_TIMESTAMP), Varint.encode(5)), new Uint8Array([1]));
	expect(() => new ObjectFormat("video").decode(inline, undefined, undefined)).toThrow(/Object Properties/);
	expect(() => new ObjectFormat("video").decode(AVC_IDR, undefined, props([[0x08, 1000n]]))).toThrow(/Timestamp/);
});

test("ObjectFormat refuses malformed properties", () => {
	const decode = (entries: [number, bigint | Uint8Array][]) =>
		new ObjectFormat("video").decode(AVC_IDR, undefined, props([...entries, [0x10, 1n]]));
	expect(() => decode([[0x08, 0n]])).toThrow(/Timescale/);
	expect(() => decode([[0x09, new Uint8Array()]])).toThrow(/Frame Marking/);
	expect(() => new ObjectFormat().decode(AVC_IDR, undefined, props([[0x10, 2n ** 53n]]))).toThrow(/2\^53/);
});

test("ObjectFormat decodes no frame from a data track's empty object", () => {
	expect(new ObjectFormat("data").decode(new Uint8Array(), undefined, undefined)).toEqual([]);
});

test("Format still reads the in-payload block, whatever Object Properties came with it", () => {
	const frame = buildFrame(concat(Varint.encode(PROP_TIMESTAMP), Varint.encode(7)), AVC_IDR);
	// Through the container interface, which hands every format the object's properties.
	const format: { decode(payload: Uint8Array, timestamp: undefined, properties: unknown): LocFrame[] } = new Format(
		"video",
	);
	const [decoded] = format.decode(frame, undefined, props([[0x10, 99n]]));
	expect(decoded.timestamp).toBe(7 as Micro);
	expect(decoded.payload).toEqual(AVC_IDR);
	expect(decoded.config).toBeUndefined();
});
