import { describe, expect, test } from "bun:test";
import * as Frame from "./frame.ts";
import * as Stream from "./stream.ts";
import { VarInt } from "./varint.ts";

describe("QMux01 record framing", () => {
	test("decodeRecord parses multiple frames concatenated in one record body", () => {
		// Build the record body the way the wire layer hands it to us:
		// frames concatenated, no leading size varint (the transport already stripped it).
		const id = new Stream.Id(VarInt.from(0n));
		const frames: Frame.Any[] = [
			{ type: "stream", id, data: new Uint8Array([1, 2, 3, 4, 5]), fin: false },
			{ type: "ping_request", sequence: 42n },
			{ type: "max_data", max: 1024n },
		];

		const parts = frames.map((f) => Frame.encode(f, "qmux-01"));
		const totalLen = parts.reduce((sum, p) => sum + p.byteLength, 0);
		const body = new Uint8Array(totalLen);
		let offset = 0;
		for (const p of parts) {
			body.set(p, offset);
			offset += p.byteLength;
		}

		const decoded = Frame.decodeRecord(body);
		expect(decoded.length).toBe(3);

		const [first, second, third] = decoded;
		expect(first.type).toBe("stream");
		if (first.type === "stream") {
			expect(Array.from(first.data)).toEqual([1, 2, 3, 4, 5]);
			expect(first.fin).toBe(false);
		}
		expect(second.type).toBe("ping_request");
		if (second.type === "ping_request") {
			expect(second.sequence).toBe(42n);
		}
		expect(third.type).toBe("max_data");
		if (third.type === "max_data") {
			expect(third.max).toBe(1024n);
		}
	});

	test("application_protocols parameter is rejected (WebSocket negotiates via subprotocol)", () => {
		// A QX_TRANSPORT_PARAMETERS frame carrying only application_protocols
		// (id 0x3d4f9c2a8b1e6075, len 0). WebSocket has its own ALPN, so this
		// parameter must never appear and decoding must throw.
		const bytes = (...vals: number[]) => new Uint8Array(vals);
		const record = bytes(
			// frame type 0x3f5153300d0a0d0a — 8-byte varint, so the high tag bits
			// make the first wire byte 0xff (the "\xffQS0\r\n\r\n" magic).
			0xff,
			0x51,
			0x53,
			0x30,
			0x0d,
			0x0a,
			0x0d,
			0x0a,
			// length = 9 (param id 8 bytes + len 1 byte)
			0x09,
			// param id 0x3d4f9c2a8b1e6075 — 8-byte varint, first wire byte 0xfd.
			0xfd,
			0x4f,
			0x9c,
			0x2a,
			0x8b,
			0x1e,
			0x60,
			0x75,
			// param length = 0
			0x00,
		);
		expect(() => Frame.decode(record, "qmux-00")).toThrow();
	});

	test("ping_request and ping_response round-trip preserves the sequence number", () => {
		const req: Frame.Any = { type: "ping_request", sequence: 0xdeadbeefn };
		const reqBytes = Frame.encode(req, "qmux-01");
		const reqDecoded = Frame.decodeRecord(reqBytes);
		expect(reqDecoded.length).toBe(1);
		expect(reqDecoded[0]).toEqual({ type: "ping_request", sequence: 0xdeadbeefn });

		const resp: Frame.Any = { type: "ping_response", sequence: 0xdeadbeefn };
		const respBytes = Frame.encode(resp, "qmux-01");
		const respDecoded = Frame.decodeRecord(respBytes);
		expect(respDecoded.length).toBe(1);
		expect(respDecoded[0]).toEqual({ type: "ping_response", sequence: 0xdeadbeefn });
	});

	test("RESET_STREAM round-trip preserves final_size", () => {
		const frame: Frame.ResetStream = {
			type: "reset_stream",
			id: new Stream.Id(VarInt.from(4n)),
			code: VarInt.from(42n),
			finalSize: 128n,
		};
		const decoded = Frame.decodeRecord(Frame.encode(frame, "qmux-01"));
		expect(decoded).toHaveLength(1);
		expect(decoded[0]).toEqual(frame);
	});

	test("datagram round-trips via the length-prefixed (0x31) form", () => {
		const frame: Frame.Any = { type: "datagram", data: new Uint8Array([1, 2, 3, 4]) };
		const bytes = Frame.encode(frame, "qmux-01");
		// 0x31, len=4, then payload.
		expect(bytes[0]).toBe(0x31);
		const decoded = Frame.decodeRecord(bytes);
		expect(decoded.length).toBe(1);
		expect(decoded[0].type).toBe("datagram");
		if (decoded[0].type === "datagram") {
			expect(Array.from(decoded[0].data)).toEqual([1, 2, 3, 4]);
		}
	});

	test("datagram no-length (0x30) form decodes (payload runs to end of record)", () => {
		// We never emit 0x30, but must accept it: 0x30 followed by the payload.
		const wire = new Uint8Array([0x30, 0x68, 0x69]);
		const decoded = Frame.decode(wire, "qmux-00");
		expect(decoded?.type).toBe("datagram");
		if (decoded?.type === "datagram") {
			expect(Array.from(decoded.data)).toEqual([0x68, 0x69]);
		}
	});

	test("max_datagram_frame_size transport parameter round-trips", () => {
		const params: Frame.Any = {
			type: "transport_parameters",
			params: { ...Frame.DEFAULT_TRANSPORT_PARAMS, initialMaxData: 1024n, maxDatagramFrameSize: 1201n },
		};
		const decoded = Frame.decodeRecord(Frame.encode(params, "qmux-01"));
		expect(decoded.length).toBe(1);
		if (decoded[0].type === "transport_parameters") {
			expect(decoded[0].params.maxDatagramFrameSize).toBe(1201n);
			expect(decoded[0].params.initialMaxData).toBe(1024n);
		}
	});

	test("decodeTransportParams seeds maxRecordSize with the draft-01 default when the parameter is omitted", () => {
		// Empty params buffer → all values default; maxRecordSize must be 16382, not 0.
		const params: Frame.TransportParameters = {
			type: "transport_parameters",
			params: {
				maxIdleTimeout: 0n,
				initialMaxData: 0n,
				initialMaxStreamDataBidiLocal: 0n,
				initialMaxStreamDataBidiRemote: 0n,
				initialMaxStreamDataUni: 0n,
				initialMaxStreamsBidi: 0n,
				initialMaxStreamsUni: 0n,
				maxDatagramFrameSize: 0n,
				// Deliberately set to 0 — exercises the encoder's "skip-if-zero" + decoder's default seeding.
				maxRecordSize: 0n,
				resetStreamAt: false,
			},
		};
		const bytes = Frame.encode(params, "qmux-01");
		const decoded = Frame.decodeRecord(bytes);
		expect(decoded.length).toBe(1);
		const got = decoded[0];
		expect(got.type).toBe("transport_parameters");
		if (got.type === "transport_parameters") {
			expect(got.params.maxRecordSize).toBe(Frame.DEFAULT_MAX_RECORD_SIZE);
		}
	});
});

describe("QMux02 (draft-02)", () => {
	test("qmux-02 shares the qmux-01 record wire format", () => {
		const id = new Stream.Id(VarInt.from(4n));
		const frames: Frame.Any[] = [
			{ type: "stream", id, data: new Uint8Array([1, 2, 3]), fin: true },
			{ type: "max_data", max: 1024n },
			{ type: "datagram", data: new Uint8Array([9, 9]) },
		];
		for (const frame of frames) {
			expect(Array.from(Frame.encode(frame, "qmux-02"))).toEqual(Array.from(Frame.encode(frame, "qmux-01")));
		}
	});

	test("RESET_STREAM_AT (0x24) decodes as a reset carrying reliableSize", () => {
		// 0x24, id=4, code=42, final_size=128 (0x40 0x80), reliable_size=64 (0x40 0x40).
		const wire = new Uint8Array([0x24, 0x04, 0x2a, 0x40, 0x80, 0x40, 0x40]);
		const decoded = Frame.decode(wire, "qmux-02");
		expect(decoded?.type).toBe("reset_stream");
		if (decoded?.type === "reset_stream") {
			expect(decoded.id.value.value).toBe(4n);
			expect(decoded.code.value).toBe(42n);
			expect(decoded.finalSize).toBe(128n);
			// reliableSize present marks it as RESET_STREAM_AT (vs a plain 0x04 reset,
			// which leaves it undefined); the session uses this to gate on negotiation.
			expect(decoded.reliableSize).toBe(64n);
		}
	});

	test("a duplicate transport parameter is rejected", () => {
		// QX_TRANSPORT_PARAMETERS carrying initial_max_data (0x04) twice — a
		// protocol error, matching the Rust decoder's DuplicateParam.
		const bytes = new Uint8Array([
			0xff,
			0x51,
			0x53,
			0x30,
			0x0d,
			0x0a,
			0x0d,
			0x0a, // frame type
			0x06, // payload length
			0x04,
			0x01,
			0x01, // id=0x04, len=1, value=1
			0x04,
			0x01,
			0x01, // duplicate
		]);
		expect(() => Frame.decode(bytes, "qmux-02")).toThrow();
	});

	test("forbidden QUIC v1 transport parameters are rejected", () => {
		for (const id of [0x00, 0x02, 0x03, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10]) {
			const bytes = new Uint8Array([
				0xff,
				0x51,
				0x53,
				0x30,
				0x0d,
				0x0a,
				0x0d,
				0x0a, // frame type
				0x02, // payload length
				id,
				0x00, // empty parameter value
			]);
			expect(() => Frame.decode(bytes, "qmux-02")).toThrow("forbidden QUIC v1 transport parameter");
		}
	});

	test("unknown extension transport parameters are ignored", () => {
		const bytes = new Uint8Array([
			0xff,
			0x51,
			0x53,
			0x30,
			0x0d,
			0x0a,
			0x0d,
			0x0a, // frame type
			0x03, // payload length
			0x11,
			0x01,
			0xff, // unknown extension id=0x11, len=1
		]);
		expect(Frame.decode(bytes, "qmux-02")?.type).toBe("transport_parameters");
	});

	test("recognized integer transport parameters require exactly one varint", () => {
		const empty = new Uint8Array([
			0xff,
			0x51,
			0x53,
			0x30,
			0x0d,
			0x0a,
			0x0d,
			0x0a,
			0x02,
			0x04,
			0x00, // initial_max_data with an empty value
		]);
		expect(() => Frame.decode(empty, "qmux-02")).toThrow();

		const trailing = new Uint8Array([
			0xff,
			0x51,
			0x53,
			0x30,
			0x0d,
			0x0a,
			0x0d,
			0x0a,
			0x04,
			0x04,
			0x02,
			0x01,
			0x00, // initial_max_data=1 followed by a trailing byte
		]);
		expect(() => Frame.decode(trailing, "qmux-02")).toThrow("trailing bytes");
	});

	test("an unknown frame type is rejected without suppressing trailing frames", () => {
		const record = new Uint8Array([0x02, 0x10, 0x01]);
		expect(() => Frame.decodeRecord(record)).toThrow("Invalid QMux frame type");
		expect(() => Frame.decode(new Uint8Array([0x02]), "qmux-02")).toThrow("Invalid QMux frame type");
	});

	test("reset_stream_at transport parameter round-trips (empty flag)", () => {
		const on: Frame.Any = {
			type: "transport_parameters",
			params: { ...Frame.DEFAULT_TRANSPORT_PARAMS, initialMaxData: 1024n, resetStreamAt: true },
		};
		const decoded = Frame.decodeRecord(Frame.encode(on, "qmux-02"));
		expect(decoded.length).toBe(1);
		if (decoded[0].type === "transport_parameters") {
			expect(decoded[0].params.resetStreamAt).toBe(true);
			expect(decoded[0].params.initialMaxData).toBe(1024n);
		}

		// Omitted from the wire (and decodes false) when unset.
		const off: Frame.Any = {
			type: "transport_parameters",
			params: { ...Frame.DEFAULT_TRANSPORT_PARAMS, initialMaxData: 1n },
		};
		const d2 = Frame.decodeRecord(Frame.encode(off, "qmux-02"));
		if (d2[0].type === "transport_parameters") {
			expect(d2[0].params.resetStreamAt).toBe(false);
		}
	});

	test("RESET_STREAM_AT with reliable_size > final_size is rejected", () => {
		// reliable_size=200 (0x40 0xc8) > final_size=128 (0x40 0x80).
		const wire = new Uint8Array([0x24, 0x04, 0x2a, 0x40, 0x80, 0x40, 0xc8]);
		expect(() => Frame.decode(wire, "qmux-02")).toThrow();
	});

	test("RESET_STREAM_AT stops at its boundary inside a record", () => {
		const resetAt = new Uint8Array([0x24, 0x04, 0x2a, 0x40, 0x80, 0x40, 0x40]);
		const stream = Frame.encode(
			{ type: "stream", id: new Stream.Id(VarInt.from(8n)), data: new Uint8Array([1, 2]), fin: false },
			"qmux-02",
		);
		const record = new Uint8Array(resetAt.byteLength + stream.byteLength);
		record.set(resetAt, 0);
		record.set(stream, resetAt.byteLength);

		const decoded = Frame.decodeRecord(record);
		expect(decoded.length).toBe(2);
		expect(decoded[0].type).toBe("reset_stream");
		expect(decoded[1].type).toBe("stream");
	});
});

describe("maxStreamPayload", () => {
	const id = Stream.Id.create(0n, Stream.Dir.Uni, true);

	/** The encoded frame fills `budget` as tightly as the varint widths allow:
	 *  one byte more would overrun it. */
	function assertTight(budget: bigint, streamId: Stream.Id, offset: bigint) {
		const payload = Frame.maxStreamPayload("qmux-01", budget, streamId, offset);
		const framed = (n: bigint) =>
			BigInt(1 + streamId.value.size() + VarInt.from(offset).size() + VarInt.from(n).size()) + n;

		expect(framed(payload) <= budget).toBe(true);
		expect(framed(payload + 1n) > budget).toBe(true);
	}

	test("fills the budget at varint boundaries", () => {
		// Where the length field changes width, subtracting the width of the space
		// left (rather than of the payload) leaves bytes unused: at 16387 that is
		// 16380, but 16382 fits — its own length still encodes in two bytes.
		for (let budget = 60n; budget <= 70n; budget++) assertTight(budget, id, 0n);
		for (let budget = 16380n; budget <= 16392n; budget++) assertTight(budget, id, 0n);
		expect(Frame.maxStreamPayload("qmux-01", 16387n, id, 0n)).toBe(16382n);
	});

	test("accounts for wider header fields", () => {
		const wide = Stream.Id.create(1n << 20n, Stream.Dir.Uni, true);
		assertTight(Frame.DEFAULT_MAX_RECORD_SIZE, wide, 1n << 20n);
		expect(Frame.maxStreamPayload("qmux-01", Frame.DEFAULT_MAX_RECORD_SIZE, wide, 1n << 20n)).toBeLessThan(
			Frame.maxStreamPayload("qmux-01", Frame.DEFAULT_MAX_RECORD_SIZE, id, 0n),
		);
	});

	test("handles the largest advertised max_record_size", () => {
		// max_record_size is a varint, so a peer may advertise 2^62-1 — a value
		// `number` cannot hold exactly.
		assertTight(VarInt.MAX, id, 0n);
	});

	test("the legacy binding reserves no length field", () => {
		// The payload runs to the end of the message, so only the type byte and the
		// stream ID come out of the budget.
		expect(Frame.maxStreamPayload("webtransport", BigInt(Frame.MAX_FRAME_SIZE), id, 0n)).toBe(
			BigInt(Frame.MAX_FRAME_SIZE) - 2n,
		);
	});
});
