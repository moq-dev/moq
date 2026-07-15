import { expect, test } from "bun:test";
import { type Time, Varint } from "@moq/net";
import { Format } from "./index.ts";

const PROP_TIMESTAMP = 0x06;
const PROP_TIMESCALE = 0x08;

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

test("Format decodes timestamp at default microseconds timescale", () => {
	const props = concat(Varint.encode(PROP_TIMESTAMP), Varint.encode(12_345));
	const payload = new Uint8Array([0xde, 0xad, 0xbe, 0xef]);
	const frame = buildFrame(props, payload);

	const fmt = new Format();
	const [decoded] = fmt.decode(frame);

	expect(decoded.timestamp).toBe(12_345 as Time.Micro);
	expect(decoded.data).toEqual(payload);
	expect(decoded.keyframe).toBe(false);
});

test("Format honors per-frame timescale property", () => {
	// timestamp = 96000 at per-frame timescale 48000 -> 2 seconds = 2_000_000 micros
	const props = concat(
		Varint.encode(PROP_TIMESTAMP),
		Varint.encode(96_000),
		Varint.encode(PROP_TIMESCALE - PROP_TIMESTAMP), // delta to 0x08
		Varint.encode(48_000),
	);
	const frame = buildFrame(props, new Uint8Array());

	const fmt = new Format();
	const [decoded] = fmt.decode(frame);

	expect(decoded.timestamp).toBe(2_000_000 as Time.Micro);
});

test("Format skips unknown odd-typed properties", () => {
	// 0x06 timestamp, then 0x0d (delta 7) video config bytes [1,2,3]
	const props = concat(
		Varint.encode(PROP_TIMESTAMP),
		Varint.encode(10),
		Varint.encode(0x0d - PROP_TIMESTAMP),
		Varint.encode(3),
		new Uint8Array([0x01, 0x02, 0x03]),
	);
	const payload = new Uint8Array([0xaa]);
	const frame = buildFrame(props, payload);

	const fmt = new Format();
	const [decoded] = fmt.decode(frame);

	expect(decoded.timestamp).toBe(10 as Time.Micro);
	expect(decoded.data).toEqual(payload);
});

test("Format throws when the timestamp property is missing", () => {
	const props = concat(Varint.encode(PROP_TIMESCALE), Varint.encode(1000));
	const frame = buildFrame(props, new Uint8Array([0xff]));

	const fmt = new Format();
	expect(() => fmt.decode(frame)).toThrow(/timestamp/);
});

test("Format rejects zero per-frame timescale", () => {
	const props = concat(
		Varint.encode(PROP_TIMESTAMP),
		Varint.encode(10),
		Varint.encode(PROP_TIMESCALE - PROP_TIMESTAMP),
		Varint.encode(0),
	);
	const frame = buildFrame(props, new Uint8Array([0xaa]));

	const fmt = new Format();
	expect(() => fmt.decode(frame)).toThrow(/timescale/);
});

test("Format throws when properties_length exceeds frame size", () => {
	const lenBytes = Varint.encode(100);
	const buf = new Uint8Array(lenBytes.byteLength + 1);
	buf.set(lenBytes, 0);
	buf[lenBytes.byteLength] = 0x06;

	const fmt = new Format();
	expect(() => fmt.decode(buf)).toThrow();
});
