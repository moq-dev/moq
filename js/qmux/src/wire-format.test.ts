import { describe, expect, test } from "bun:test";
import * as Frame from "./frame.ts";
import * as Stream from "./stream.ts";
import { VarInt } from "./varint.ts";

// Byte-level wire-format fixtures for the legacy `webtransport` and QMux00 formats.
//
// Each test hard-codes the exact bytes a peer would put on the wire and verifies:
//   1. The current decoder parses them into the expected Frame value.
//   2. The current encoder produces the same bytes from the same Frame value.
//
// Mirrors `rs/qmux/tests/wire_format.rs` — the Rust and TS implementations must
// agree on the bytes, otherwise the JS polyfill can't interop with Rust peers.

function bytes(...arr: number[]): Uint8Array {
	return new Uint8Array(arr);
}

function sid(v: bigint): Stream.Id {
	return new Stream.Id(VarInt.from(v));
}

function code(v: bigint): VarInt {
	return VarInt.from(v);
}

function arr(u8: Uint8Array): number[] {
	return Array.from(u8);
}

describe("WebTransport wire format", () => {
	test("stream (no fin)", () => {
		// 0x08 + id_varint(=4) + payload("hi")
		const wire = bytes(0x08, 0x04, 0x68, 0x69);
		const frame: Frame.Data = {
			type: "stream",
			id: sid(4n),
			data: bytes(0x68, 0x69),
			fin: false,
		};
		const decoded = Frame.decode(wire, "webtransport") as Frame.Data;
		expect(decoded.type).toBe("stream");
		expect(decoded.id.value.value).toBe(4n);
		expect(arr(decoded.data)).toEqual([0x68, 0x69]);
		expect(decoded.fin).toBe(false);

		expect(arr(Frame.encode(frame, "webtransport"))).toEqual(arr(wire));
	});

	test("stream (fin)", () => {
		const wire = bytes(0x09, 0x08, 0x62, 0x79, 0x65);
		const frame: Frame.Data = {
			type: "stream",
			id: sid(8n),
			data: bytes(0x62, 0x79, 0x65),
			fin: true,
		};
		const decoded = Frame.decode(wire, "webtransport") as Frame.Data;
		expect(decoded.type).toBe("stream");
		expect(decoded.id.value.value).toBe(8n);
		expect(arr(decoded.data)).toEqual([0x62, 0x79, 0x65]);
		expect(decoded.fin).toBe(true);

		expect(arr(Frame.encode(frame, "webtransport"))).toEqual(arr(wire));
	});

	test("reset_stream", () => {
		// 0x04 + id(=4) + code(=42). WebTransport carries no final_size on the wire.
		const wire = bytes(0x04, 0x04, 0x2a);
		const frame: Frame.ResetStream = {
			type: "reset_stream",
			id: sid(4n),
			code: code(42n),
			finalSize: 0n,
		};
		const decoded = Frame.decode(wire, "webtransport") as Frame.ResetStream;
		expect(decoded.type).toBe("reset_stream");
		expect(decoded.id.value.value).toBe(4n);
		expect(decoded.code.value).toBe(42n);

		expect(arr(Frame.encode(frame, "webtransport"))).toEqual(arr(wire));
	});

	test("stop_sending", () => {
		const wire = bytes(0x05, 0x04, 0x2a);
		const frame: Frame.StopSending = {
			type: "stop_sending",
			id: sid(4n),
			code: code(42n),
		};
		const decoded = Frame.decode(wire, "webtransport") as Frame.StopSending;
		expect(decoded.type).toBe("stop_sending");
		expect(decoded.id.value.value).toBe(4n);
		expect(decoded.code.value).toBe(42n);

		expect(arr(Frame.encode(frame, "webtransport"))).toEqual(arr(wire));
	});

	test("application_close", () => {
		// 0x1d + code(=42) + reason("bye") as the rest of the buffer.
		const wire = bytes(0x1d, 0x2a, 0x62, 0x79, 0x65);
		const frame: Frame.ApplicationClose = {
			type: "application_close",
			code: code(42n),
			reason: "bye",
		};
		const decoded = Frame.decode(wire, "webtransport") as Frame.ApplicationClose;
		expect(decoded.type).toBe("application_close");
		expect(decoded.code.value).toBe(42n);
		expect(decoded.reason).toBe("bye");

		expect(arr(Frame.encode(frame, "webtransport"))).toEqual(arr(wire));
	});

	test("connection_close (transport, 0x1c)", () => {
		// Same legacy body, but the leading byte flips to 0x1c and it decodes to a
		// distinct transport-close frame so the receiver rejects.
		const wire = bytes(0x1c, 0x2a, 0x62, 0x79, 0x65);
		const frame: Frame.ConnectionClose = {
			type: "connection_close",
			code: code(42n),
			reason: "bye",
		};
		const decoded = Frame.decode(wire, "webtransport") as Frame.ConnectionClose;
		expect(decoded.type).toBe("connection_close");

		expect(arr(Frame.encode(frame, "webtransport"))).toEqual(arr(wire));
	});
});

describe("QMux draft-00 wire format", () => {
	test("stream with LEN bit (no fin)", () => {
		// 0x0e = STREAM | OFF | LEN, id=4, offset=5, len=2, "hi"
		const wire = bytes(0x0e, 0x04, 0x05, 0x02, 0x68, 0x69);
		const frame: Frame.Data = {
			type: "stream",
			id: sid(4n),
			offset: 5n,
			data: bytes(0x68, 0x69),
			fin: false,
		};
		const decoded = Frame.decode(wire, "qmux-00") as Frame.Data;
		expect(decoded.type).toBe("stream");
		expect(decoded.id.value.value).toBe(4n);
		expect(decoded.offset).toBe(5n);
		expect(arr(decoded.data)).toEqual([0x68, 0x69]);
		expect(decoded.fin).toBe(false);

		expect(arr(Frame.encode(frame, "qmux-00"))).toEqual(arr(wire));
	});

	test("stream with LEN and FIN bits", () => {
		// 0x0f = STREAM | OFF | LEN | FIN, offset=0
		const wire = bytes(0x0f, 0x08, 0x00, 0x03, 0x62, 0x79, 0x65);
		const frame: Frame.Data = {
			type: "stream",
			id: sid(8n),
			data: bytes(0x62, 0x79, 0x65),
			fin: true,
		};
		const decoded = Frame.decode(wire, "qmux-00") as Frame.Data;
		expect(decoded.type).toBe("stream");
		expect(decoded.id.value.value).toBe(8n);
		expect(decoded.offset).toBe(0n);
		expect(arr(decoded.data)).toEqual([0x62, 0x79, 0x65]);
		expect(decoded.fin).toBe(true);

		expect(arr(Frame.encode(frame, "qmux-00"))).toEqual(arr(wire));
	});

	test("stream without OFF still decodes as offset zero", () => {
		const decoded = Frame.decode(bytes(0x0a, 0x04, 0x02, 0x68, 0x69), "qmux-00") as Frame.Data;
		expect(decoded.offset).toBe(0n);
		expect(arr(decoded.data)).toEqual([0x68, 0x69]);
	});

	test("max_data (2-byte varint payload)", () => {
		// 0x10 + max(=1024 = 0x44 0x00)
		const wire = bytes(0x10, 0x44, 0x00);
		const frame: Frame.MaxData = { type: "max_data", max: 1024n };
		const decoded = Frame.decode(wire, "qmux-00") as Frame.MaxData;
		expect(decoded.type).toBe("max_data");
		expect(decoded.max).toBe(1024n);

		expect(arr(Frame.encode(frame, "qmux-00"))).toEqual(arr(wire));
	});

	test("max_stream_data", () => {
		const wire = bytes(0x11, 0x04, 0x44, 0x00);
		const frame: Frame.MaxStreamData = { type: "max_stream_data", id: sid(4n), max: 1024n };
		const decoded = Frame.decode(wire, "qmux-00") as Frame.MaxStreamData;
		expect(decoded.type).toBe("max_stream_data");
		expect(decoded.id.value.value).toBe(4n);
		expect(decoded.max).toBe(1024n);

		expect(arr(Frame.encode(frame, "qmux-00"))).toEqual(arr(wire));
	});

	test("application_close", () => {
		// 0x1d + code(=42) + reason_len(=3) + "bye". No Frame Type field — RFC 9000
		// §19.19 omits it for the application variant.
		const wire = bytes(0x1d, 0x2a, 0x03, 0x62, 0x79, 0x65);
		const frame: Frame.ApplicationClose = {
			type: "application_close",
			code: code(42n),
			reason: "bye",
		};
		const decoded = Frame.decode(wire, "qmux-00") as Frame.ApplicationClose;
		expect(decoded.type).toBe("application_close");
		expect(decoded.code.value).toBe(42n);
		expect(decoded.reason).toBe("bye");

		expect(arr(Frame.encode(frame, "qmux-00"))).toEqual(arr(wire));
	});

	test("connection_close (transport, 0x1c)", () => {
		// 0x1c + code(=42) + frame_type(=0) + reason_len(=3) + "bye" — a transport
		// error / protocol violation. The transport variant carries the Frame Type.
		const wire = bytes(0x1c, 0x2a, 0x00, 0x03, 0x62, 0x79, 0x65);
		const frame: Frame.ConnectionClose = {
			type: "connection_close",
			code: code(42n),
			reason: "bye",
		};
		const decoded = Frame.decode(wire, "qmux-00") as Frame.ConnectionClose;
		expect(decoded.type).toBe("connection_close");

		expect(arr(Frame.encode(frame, "qmux-00"))).toEqual(arr(wire));
	});

	test("transport_parameters carrying just initial_max_data=1024", () => {
		// Frame type = QX_TRANSPORT_PARAMETERS (8-byte varint 0xff 0x51 0x53 0x30 0x0d 0x0a 0x0d 0x0a)
		// Payload length varint = 4
		// Payload: id(0x04) + len(0x02) + value(0x44 0x00) → 4 bytes
		const wire = bytes(0xff, 0x51, 0x53, 0x30, 0x0d, 0x0a, 0x0d, 0x0a, 0x04, 0x04, 0x02, 0x44, 0x00);

		const decoded = Frame.decode(wire, "qmux-00") as Frame.TransportParameters;
		expect(decoded.type).toBe("transport_parameters");
		expect(decoded.params.initialMaxData).toBe(1024n);
		// `maxRecordSize` is seeded from DEFAULT_TRANSPORT_PARAMS on decode (16382), per draft-01.
		expect(decoded.params.maxRecordSize).toBe(Frame.DEFAULT_MAX_RECORD_SIZE);

		// Re-encoding must round-trip. Clear maxRecordSize so the encoder skips it —
		// a peer that didn't send the parameter wouldn't have it set, and the encoder
		// only writes non-zero params.
		const frame: Frame.TransportParameters = {
			type: "transport_parameters",
			params: {
				...decoded.params,
				maxRecordSize: 0n,
			},
		};
		expect(arr(Frame.encode(frame, "qmux-00"))).toEqual(arr(wire));
	});

	test("QMux00 encoding does NOT prepend a record size varint", () => {
		// Regression guard for the WebSocket record-framing fix.
		const cases: [Frame.Any, number][] = [
			[{ type: "stream", id: sid(4n), data: bytes(0x68, 0x69), fin: false }, 0x0e],
			[{ type: "max_data", max: 1024n }, 0x10],
			[{ type: "application_close", code: code(42n), reason: "bye" }, 0x1d],
		];
		for (const [frame, expectedFirstByte] of cases) {
			const encoded = Frame.encode(frame, "qmux-00");
			expect(encoded[0]).toBe(expectedFirstByte);
		}
	});
});
