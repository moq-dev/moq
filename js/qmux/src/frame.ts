import * as Stream from "./stream.ts";
import { VarInt } from "./varint.ts";

/** Wire format that frame encode/decode operates on.
 *
 * Internal to the qmux crate. The public `Version` type (exported from
 * `index.ts`) only includes the QMux drafts; `"webtransport"` is the
 * legacy fallback wire format and isn't a valid value for any public option.
 */
export type WireFormat = "webtransport" | "qmux-00" | "qmux-01" | "qmux-02";

/** Maximum size of a single QMux frame on the wire.
 *
 * This is draft-00's `max_frame_size`, which bounds the whole frame. The
 * record-framed drafts supersede it with the negotiated `max_record_size`, so it
 * only bounds draft-00 and the legacy WebTransport binding. */
export const MAX_FRAME_SIZE = 16384;

/** Send-only compatibility ceiling for STREAM payloads on the currently
 * supported wire formats.
 *
 * Released peers historically enforce this value on receive. A future QMux wire
 * version can remove the ceiling once it cannot negotiate with those peers.
 * Never use this value to validate inbound frames. */
export const MAX_FRAME_PAYLOAD = MAX_FRAME_SIZE - 32;

/** Largest value each varint width encodes (RFC 9000 §16). */
const VARINT_MAX = [63n, 16383n, 1073741823n, 4611686018427387903n];

/** Largest `n` with `n + varint(n).size() <= available`.
 *
 * The length varint's width depends on the payload it describes, so this is a
 * fixed point rather than a subtraction: at `available = 16384`, reserving four
 * bytes (the width of 16384 itself) yields 16380, but 16382 fits — its length
 * still encodes in two. Solve it by asking, for each width, what the largest
 * payload that width can describe *and* leave room for is, then take the best. */
function maxLengthPrefixedPayload(available: bigint): bigint {
	let best = 0n;
	for (const max of VARINT_MAX) {
		const width = BigInt(VarInt.from(max).size());
		if (available < width) break;
		const candidate = available - width < max ? available - width : max;
		// Skip a candidate this width cannot describe: a narrower varint means a
		// smaller width already covered it, exactly.
		if (VarInt.from(candidate).size() === Number(width) && candidate > best) best = candidate;
	}
	return best;
}

/** Largest STREAM payload that keeps the encoded frame within `budget` bytes.
 *
 * The budget is what the peer accepts for one frame — its `max_record_size` on
 * the record-framed drafts, {@link MAX_FRAME_SIZE} otherwise — so the frame's own
 * header comes out of it: the type, the stream ID, and (QMux only) the offset and
 * the length varint. Header widths are the ones this frame actually encodes, not
 * a worst-case reservation. The session applies any send-only compatibility
 * ceiling after calculating this exact peer budget.
 *
 * `budget` and the result are `bigint`: `max_record_size` is a varint, so a peer
 * may advertise up to 2^62-1, which `number` cannot hold exactly. */
export function maxStreamPayload(version: WireFormat, budget: bigint, id: Stream.Id, offset: bigint): bigint {
	// The type is 0x0e/0x0f on QMux and 0x08/0x09 on the legacy binding: one byte
	// either way.
	const header = BigInt(1 + id.value.size());
	if (version === "webtransport") {
		// The payload runs to the end of the message, so there is no length varint
		// to reserve.
		return budget > header ? budget - header : 0n;
	}

	const fixed = header + BigInt(VarInt.from(offset).size());
	return maxLengthPrefixedPayload(budget > fixed ? budget - fixed : 0n);
}

export interface Data {
	type: "stream";
	id: Stream.Id;
	/** Byte offset of this payload. QMux senders always encode it; receivers
	 * preserve it without enforcing continuity yet. Defaults to zero when omitted. */
	offset?: bigint;
	data: Uint8Array;
	fin: boolean;
}

export interface ResetStream {
	type: "reset_stream";
	id: Stream.Id;
	code: VarInt;
	/** Final byte offset consumed by the send side. The legacy WebTransport wire
	 * format omits this field and decodes it as zero. */
	finalSize: bigint;
	/** Set (to the frame's Reliable Size) when this was decoded from a
	 * RESET_STREAM_AT frame (0x24, draft-02); absent for a plain RESET_STREAM.
	 * The session uses its presence to enforce that RESET_STREAM_AT is only
	 * accepted when we advertised the `reset_stream_at` transport parameter. */
	reliableSize?: bigint;
}

export interface StopSending {
	type: "stop_sending";
	id: Stream.Id;
	code: VarInt;
}

/** Transport CONNECTION_CLOSE (0x1c): a protocol violation or transport error the
 *  sender detected. The receiver *rejects* `closed`. Per RFC 9000 §19.19 the
 *  transport variant carries the Frame Type field (we always send 0). */
export interface ConnectionClose {
	type: "connection_close";
	code: VarInt;
	reason: string;
}

/** APPLICATION_CLOSE (0x1d): a graceful, app-initiated close carrying an
 *  application code/reason. The receiver *fulfills* `closed`. Per RFC 9000 §19.19
 *  the application variant omits the Frame Type field.
 *
 *  The close subtype, not the code, carries the fulfill/reject distinction —
 *  codes are application-defined, so a graceful close may use any value. */
export interface ApplicationClose {
	type: "application_close";
	code: VarInt;
	reason: string;
}

export interface MaxData {
	type: "max_data";
	max: bigint;
}

export interface MaxStreamData {
	type: "max_stream_data";
	id: Stream.Id;
	max: bigint;
}

export interface MaxStreamsBidi {
	type: "max_streams_bidi";
	max: bigint;
}

export interface MaxStreamsUni {
	type: "max_streams_uni";
	max: bigint;
}

export interface DataBlocked {
	type: "data_blocked";
	limit: bigint;
}

export interface StreamDataBlocked {
	type: "stream_data_blocked";
	id: Stream.Id;
	limit: bigint;
}

export interface StreamsBlockedBidi {
	type: "streams_blocked_bidi";
	limit: bigint;
}

export interface StreamsBlockedUni {
	type: "streams_blocked_uni";
	limit: bigint;
}

export interface TransportParameters {
	type: "transport_parameters";
	params: TransportParams;
}

/** An unreliable datagram (RFC 9221). */
export interface Datagram {
	type: "datagram";
	data: Uint8Array;
	/** Whether the frame carried an explicit length varint on the wire (the
	 * `0x31` form) rather than the no-length `0x30` form, whose payload is
	 * delimited by the enclosing record. We always *emit* `0x31`; decoders set
	 * this so the receive path can size the frame exactly for
	 * `max_datagram_frame_size` validation. Absent (encode side) means `0x31`. */
	lengthPrefixed?: boolean;
}

export interface TransportParams {
	maxIdleTimeout: bigint;
	initialMaxData: bigint;
	initialMaxStreamDataBidiLocal: bigint;
	initialMaxStreamDataBidiRemote: bigint;
	initialMaxStreamDataUni: bigint;
	initialMaxStreamsBidi: bigint;
	initialMaxStreamsUni: bigint;
	/** RFC 9221 max_datagram_frame_size (ID 0x20); 0 = datagrams unsupported. */
	maxDatagramFrameSize: bigint;
	maxRecordSize: bigint;
	/** Whether we advertise support for *receiving* RESET_STREAM_AT frames, via
	 * the empty-valued `reset_stream_at` transport parameter (ID 0x1d). Only set
	 * on QMux draft-02. */
	resetStreamAt: boolean;
}

/** Default max_record_size per draft-01. */
export const DEFAULT_MAX_RECORD_SIZE = 16382n;

export const DEFAULT_TRANSPORT_PARAMS: TransportParams = {
	maxIdleTimeout: 0n,
	initialMaxData: 0n,
	initialMaxStreamDataBidiLocal: 0n,
	initialMaxStreamDataBidiRemote: 0n,
	initialMaxStreamDataUni: 0n,
	initialMaxStreamsBidi: 0n,
	initialMaxStreamsUni: 0n,
	maxDatagramFrameSize: 0n,
	maxRecordSize: DEFAULT_MAX_RECORD_SIZE,
	resetStreamAt: false,
};

export const RECOMMENDED_TRANSPORT_PARAMS: TransportParams = {
	maxIdleTimeout: 30_000n,
	initialMaxData: 1_048_576n,
	initialMaxStreamDataBidiLocal: 262_144n,
	initialMaxStreamDataBidiRemote: 262_144n,
	initialMaxStreamDataUni: 262_144n,
	initialMaxStreamsBidi: 100n,
	initialMaxStreamsUni: 100n,
	maxDatagramFrameSize: DEFAULT_MAX_RECORD_SIZE,
	maxRecordSize: DEFAULT_MAX_RECORD_SIZE,
	resetStreamAt: false,
};

export interface Padding {
	type: "padding";
}

/** QX_PING frame (draft-01) */
export interface PingRequest {
	type: "ping_request";
	sequence: bigint;
}

export interface PingResponse {
	type: "ping_response";
	sequence: bigint;
}

export type Any =
	| Data
	| ResetStream
	| StopSending
	| ConnectionClose
	| ApplicationClose
	| MaxData
	| MaxStreamData
	| MaxStreamsBidi
	| MaxStreamsUni
	| DataBlocked
	| StreamDataBlocked
	| StreamsBlockedBidi
	| StreamsBlockedUni
	| TransportParameters
	| PingRequest
	| PingResponse
	| Datagram;

export function encode(frame: Any, version: WireFormat = "webtransport"): Uint8Array {
	if (version === "webtransport") {
		return encodeWebTransport(frame);
	}
	return encodeQMux(frame);
}

/** Returns true if the version uses QMux framing (draft-00 or later). */
export function isQmux(version: WireFormat): boolean {
	return version === "qmux-00" || version === "qmux-01" || version === "qmux-02";
}

/** Returns true if the version uses the QMux Record framing layer and the
 * features introduced alongside it (QX_PING keep-alive, PADDING, datagrams,
 * max_record_size) — draft-01 and later. */
export function usesRecords(version: WireFormat): boolean {
	return version === "qmux-01" || version === "qmux-02";
}

// QX_PING frame type constants (draft-01)
const QX_PING_REQUEST = 0x348c67529ef8c7bdn;
const QX_PING_RESPONSE = 0x348c67529ef8c7ben;
// RESET_STREAM_AT (draft-ietf-quic-reliable-stream-reset), permitted by QMux
// draft-02. Decode-only: accepted, validated, and treated as a plain reset.
const RESET_STREAM_AT = 0x24n;
// max_record_size transport parameter ID
const MAX_RECORD_SIZE_ID = 0x0571c59429cd0845n;
// application_protocols transport parameter ID (QMux-specific, non-TLS ALPN).
// This implementation only runs over WebSocket, which negotiates the protocol
// via its subprotocol (ALPN), so receiving this parameter is a protocol error.
const APPLICATION_PROTOCOLS_ID = 0x3d4f9c2a8b1e6075n;
// reset_stream_at transport parameter ID (draft-ietf-quic-reliable-stream-reset):
// empty value, presence advertises support for receiving RESET_STREAM_AT.
const RESET_STREAM_AT_PARAM_ID = 0x1dn;

// The transport-parameter IDs we recognize. A repeat of any of these is a
// protocol error (matching the Rust decoder's DuplicateParam); unknown IDs are
// skipped and may repeat, per RFC 9000.
const RECOGNIZED_PARAM_IDS = new Set<bigint>([
	0x01n,
	0x04n,
	0x05n,
	0x06n,
	0x07n,
	0x08n,
	0x09n,
	0x20n,
	MAX_RECORD_SIZE_ID,
	APPLICATION_PROTOCOLS_ID,
	RESET_STREAM_AT_PARAM_ID,
]);

function encodeWebTransport(frame: Any): Uint8Array {
	switch (frame.type) {
		case "stream": {
			let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + frame.data.length), 0, 1);

			buffer[0] = frame.fin ? 0x09 : 0x08;
			buffer = frame.id.value.encode(buffer);

			buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + frame.data.length);
			buffer.set(frame.data, buffer.byteLength - frame.data.length);

			return buffer;
		}

		case "reset_stream": {
			let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + 8), 0, 1);

			buffer[0] = 0x04;
			buffer = frame.id.value.encode(buffer);
			buffer = frame.code.encode(buffer);
			return buffer;
		}

		case "stop_sending": {
			let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + 8), 0, 1);

			buffer[0] = 0x05;
			buffer = frame.id.value.encode(buffer);
			buffer = frame.code.encode(buffer);
			return buffer;
		}

		// The legacy WebTransport format keeps the reason as the rest of the buffer
		// (no Frame Type / length fields); only the type byte differs.
		case "connection_close":
		case "application_close": {
			const body = new TextEncoder().encode(frame.reason);
			let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + body.length), 0, 1);

			buffer[0] = frame.type === "application_close" ? 0x1d : 0x1c;
			buffer = frame.code.encode(buffer);

			buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + body.length);
			buffer.set(body, buffer.byteLength - body.length);

			return buffer;
		}

		default:
			throw new Error("flow control frames are not supported in WebTransport version");
	}
}

function encodeQMux(frame: Any): Uint8Array {
	switch (frame.type) {
		case "stream": {
			// Always set OFF (0x04) and LEN (0x02). Continuity validation remains
			// deferred on the receive path.
			const frameType = VarInt.from(0x0e | (frame.fin ? 0x01 : 0x00));
			const offsetVi = VarInt.from(frame.offset ?? 0n);
			const lengthVi = VarInt.from(frame.data.length);

			const maxSize = 8 + 8 + 8 + 8 + frame.data.length;
			let buffer = new Uint8Array(new ArrayBuffer(maxSize), 0, 0);

			buffer = frameType.encode(buffer);
			buffer = frame.id.value.encode(buffer);
			buffer = offsetVi.encode(buffer);
			buffer = lengthVi.encode(buffer);

			buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + frame.data.length);
			buffer.set(frame.data, buffer.byteLength - frame.data.length);

			return buffer;
		}

		case "reset_stream": {
			const frameType = VarInt.from(0x04);
			const finalSize = VarInt.from(frame.finalSize);

			let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + 8 + 8), 0, 0);

			buffer = frameType.encode(buffer);
			buffer = frame.id.value.encode(buffer);
			buffer = frame.code.encode(buffer);
			buffer = finalSize.encode(buffer);
			return buffer;
		}

		case "stop_sending": {
			const frameType = VarInt.from(0x05);

			let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + 8), 0, 0);

			buffer = frameType.encode(buffer);
			buffer = frame.id.value.encode(buffer);
			buffer = frame.code.encode(buffer);
			return buffer;
		}

		case "connection_close": {
			// CONNECTION_CLOSE (0x1c) carries the Frame Type field (RFC 9000 §19.19).
			// We don't track which frame tripped the error, so it's always 0 (PADDING).
			const frameType = VarInt.from(0x1c);
			const causingFrameType = VarInt.from(0);
			const body = new TextEncoder().encode(frame.reason);
			const reasonLength = VarInt.from(body.length);

			let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + 8 + 8 + body.length), 0, 0);

			buffer = frameType.encode(buffer);
			buffer = frame.code.encode(buffer);
			buffer = causingFrameType.encode(buffer);
			buffer = reasonLength.encode(buffer);

			buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + body.length);
			buffer.set(body, buffer.byteLength - body.length);

			return buffer;
		}

		case "application_close": {
			// APPLICATION_CLOSE (0x1d): no Frame Type field (RFC 9000 §19.19).
			const frameType = VarInt.from(0x1d);
			const body = new TextEncoder().encode(frame.reason);
			const reasonLength = VarInt.from(body.length);

			let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + 8 + body.length), 0, 0);

			buffer = frameType.encode(buffer);
			buffer = frame.code.encode(buffer);
			buffer = reasonLength.encode(buffer);

			buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + body.length);
			buffer.set(body, buffer.byteLength - body.length);

			return buffer;
		}

		case "max_data": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(0x10).encode(buffer);
			buffer = VarInt.from(frame.max).encode(buffer);
			return buffer;
		}

		case "max_stream_data": {
			let buffer = new Uint8Array(new ArrayBuffer(24), 0, 0);
			buffer = VarInt.from(0x11).encode(buffer);
			buffer = frame.id.value.encode(buffer);
			buffer = VarInt.from(frame.max).encode(buffer);
			return buffer;
		}

		case "max_streams_bidi": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(0x12).encode(buffer);
			buffer = VarInt.from(frame.max).encode(buffer);
			return buffer;
		}

		case "max_streams_uni": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(0x13).encode(buffer);
			buffer = VarInt.from(frame.max).encode(buffer);
			return buffer;
		}

		case "data_blocked": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(0x14).encode(buffer);
			buffer = VarInt.from(frame.limit).encode(buffer);
			return buffer;
		}

		case "stream_data_blocked": {
			let buffer = new Uint8Array(new ArrayBuffer(24), 0, 0);
			buffer = VarInt.from(0x15).encode(buffer);
			buffer = frame.id.value.encode(buffer);
			buffer = VarInt.from(frame.limit).encode(buffer);
			return buffer;
		}

		case "streams_blocked_bidi": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(0x16).encode(buffer);
			buffer = VarInt.from(frame.limit).encode(buffer);
			return buffer;
		}

		case "streams_blocked_uni": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(0x17).encode(buffer);
			buffer = VarInt.from(frame.limit).encode(buffer);
			return buffer;
		}

		case "transport_parameters": {
			const payload = encodeTransportParams(frame.params);
			let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + payload.byteLength), 0, 0);
			buffer = VarInt.from(0x3f5153300d0a0d0an).encode(buffer);
			buffer = VarInt.from(payload.byteLength).encode(buffer);
			buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + payload.byteLength);
			buffer.set(payload, buffer.byteLength - payload.byteLength);
			return buffer;
		}

		case "ping_request": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(QX_PING_REQUEST).encode(buffer);
			buffer = VarInt.from(frame.sequence).encode(buffer);
			return buffer;
		}

		case "ping_response": {
			let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
			buffer = VarInt.from(QX_PING_RESPONSE).encode(buffer);
			buffer = VarInt.from(frame.sequence).encode(buffer);
			return buffer;
		}

		case "datagram": {
			// Length-prefixed form (0x31). Datagrams are only sent on QMux01, where
			// the record (WS message) boundary would already delimit a lone 0x30
			// datagram — but we emit 0x31 so it stays self-delimiting even if a
			// record ever carries a frame after it. The length costs 1-2 bytes.
			const lengthVi = VarInt.from(frame.data.length);
			let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + frame.data.length), 0, 0);
			buffer = VarInt.from(0x31).encode(buffer);
			buffer = lengthVi.encode(buffer);
			buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + frame.data.length);
			buffer.set(frame.data, buffer.byteLength - frame.data.length);
			return buffer;
		}
	}
}

function encodeTransportParams(params: TransportParams): Uint8Array<ArrayBuffer> {
	// Each param: id(varint) + length(varint) + value(varint)
	// Max 9 varint params * 24 bytes each + the 2-byte reset_stream_at flag.
	let buffer = new Uint8Array(new ArrayBuffer(256), 0, 0);

	function writeParam(buf: Uint8Array<ArrayBuffer>, id: number | bigint, value: bigint): Uint8Array<ArrayBuffer> {
		if (value === 0n) return buf;
		const valVi = VarInt.from(value);
		buf = VarInt.from(id).encode(buf);
		buf = VarInt.from(valVi.size()).encode(buf);
		buf = valVi.encode(buf);
		return buf;
	}

	buffer = writeParam(buffer, 0x01, params.maxIdleTimeout);
	buffer = writeParam(buffer, 0x04, params.initialMaxData);
	buffer = writeParam(buffer, 0x05, params.initialMaxStreamDataBidiLocal);
	buffer = writeParam(buffer, 0x06, params.initialMaxStreamDataBidiRemote);
	buffer = writeParam(buffer, 0x07, params.initialMaxStreamDataUni);
	buffer = writeParam(buffer, 0x08, params.initialMaxStreamsBidi);
	buffer = writeParam(buffer, 0x09, params.initialMaxStreamsUni);
	buffer = writeParam(buffer, 0x20, params.maxDatagramFrameSize);
	buffer = writeParam(buffer, MAX_RECORD_SIZE_ID, params.maxRecordSize);

	// reset_stream_at: an empty-valued flag. Presence advertises that we accept
	// RESET_STREAM_AT frames; absence means we don't.
	if (params.resetStreamAt) {
		buffer = VarInt.from(RESET_STREAM_AT_PARAM_ID).encode(buffer);
		buffer = VarInt.from(0).encode(buffer);
	}

	return buffer;
}

function decodeTransportParams(buffer: Uint8Array): TransportParams {
	const params = { ...DEFAULT_TRANSPORT_PARAMS };
	const seen = new Set<bigint>();
	let v: VarInt;

	while (buffer.byteLength > 0) {
		[v, buffer] = VarInt.decode(buffer);
		const id = v.value;

		[v, buffer] = VarInt.decode(buffer);
		const len = Number(v.value);

		if (buffer.byteLength < len) {
			throw new Error("transport parameter truncated");
		}

		const paramData = buffer.slice(0, len);
		buffer = buffer.slice(len);

		// A repeated recognized parameter is a protocol error (mirrors Rust's
		// DuplicateParam). Unknown IDs fall through and may repeat.
		if (RECOGNIZED_PARAM_IDS.has(id)) {
			if (seen.has(id)) {
				throw new Error(`duplicate transport parameter 0x${id.toString(16)}`);
			}
			seen.add(id);
		}

		// In-band ALPN negotiation is only valid on transports without their own
		// (TCP, Unix sockets). This implementation only runs over WebSocket, which
		// negotiates via its subprotocol, so the parameter must never appear.
		if (id === APPLICATION_PROTOCOLS_ID) {
			throw new Error("unexpected application_protocols parameter over WebSocket");
		}

		// reset_stream_at: presence-only. A non-empty value is a
		// TRANSPORT_PARAMETER_ERROR per the extension spec.
		if (id === RESET_STREAM_AT_PARAM_ID) {
			if (paramData.byteLength !== 0) {
				throw new Error("reset_stream_at transport parameter must be empty");
			}
			params.resetStreamAt = true;
			continue;
		}

		// QMux permits only a subset of the transport parameters defined by
		// QUIC v1. Unassigned IDs are extensions and remain ignorable.
		if (id === 0x00n || id === 0x02n || id === 0x03n || (id >= 0x0an && id <= 0x10n)) {
			throw new Error(`forbidden QUIC v1 transport parameter 0x${id.toString(16)}`);
		}

		if (!RECOGNIZED_PARAM_IDS.has(id)) {
			continue;
		}

		const [paramValueVarInt, remaining] = VarInt.decode(paramData);
		if (remaining.byteLength !== 0) {
			throw new Error(`transport parameter 0x${id.toString(16)} has trailing bytes`);
		}
		const paramValue = paramValueVarInt.value;

		switch (id) {
			case 0x01n:
				params.maxIdleTimeout = paramValue;
				break;
			case 0x04n:
				params.initialMaxData = paramValue;
				break;
			case 0x05n:
				params.initialMaxStreamDataBidiLocal = paramValue;
				break;
			case 0x06n:
				params.initialMaxStreamDataBidiRemote = paramValue;
				break;
			case 0x07n:
				params.initialMaxStreamDataUni = paramValue;
				break;
			case 0x08n:
				params.initialMaxStreamsBidi = paramValue;
				break;
			case 0x09n:
				params.initialMaxStreamsUni = paramValue;
				break;
			case 0x20n:
				params.maxDatagramFrameSize = paramValue;
				break;
			case MAX_RECORD_SIZE_ID:
				params.maxRecordSize = paramValue;
				break;
			default:
				throw new Error(`unhandled recognized transport parameter 0x${id.toString(16)}`);
		}
	}

	return params;
}

export function decode(buffer: Uint8Array, version: WireFormat = "webtransport"): Any | null {
	if (buffer.length === 0) {
		throw new Error("Invalid frame: empty buffer");
	}

	if (version === "webtransport") {
		return decodeWebTransport(buffer);
	}
	return decodeQMux(buffer);
}

/** Slice `len` bytes off the front of `buffer`, rejecting truncated input.
 *
 * `Uint8Array.slice` clamps to the buffer end and silently returns short data,
 * which is the wrong behavior for parsing length-prefixed wire formats. Use
 * `take` at every site that reads a varint-declared payload length.
 */
function take(buffer: Uint8Array, len: number): [Uint8Array, Uint8Array] {
	if (buffer.byteLength < len) {
		throw new Error(`frame truncated: need ${len} bytes, have ${buffer.byteLength}`);
	}
	return [buffer.slice(0, len), buffer.slice(len)];
}

/** Decode all frames from a QMux Record payload (draft-01).
 * A record contains one or more frames concatenated together.
 */
export function decodeRecord(buffer: Uint8Array): Any[] {
	const frames: Any[] = [];
	while (buffer.byteLength > 0) {
		const result = decodeQMuxOne(buffer);
		if (result === null) break;
		const [frame, remaining] = result;
		if (frame !== null) {
			frames.push(frame);
		}
		buffer = remaining;
	}
	return frames;
}

function decodeWebTransport(buffer: Uint8Array): Any {
	const frameType = buffer[0];
	buffer = buffer.slice(1);

	let v: VarInt;

	if (frameType === 0x04) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		return { type: "reset_stream", id, code, finalSize: 0n };
	}

	if (frameType === 0x05) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		return { type: "stop_sending", id, code };
	}

	if (frameType === 0x1d || frameType === 0x1c) {
		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		// Legacy format: the reason is the rest of the buffer (no Frame Type field).
		const reason = new TextDecoder().decode(buffer);

		return frameType === 0x1d
			? { type: "application_close", code, reason }
			: { type: "connection_close", code, reason };
	}

	if (frameType === 0x08 || frameType === 0x09) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		return {
			type: "stream",
			id,
			data: buffer,
			fin: frameType === 0x09,
		};
	}

	throw new Error(`Invalid frame type: ${frameType}`);
}

function decodeQMux(buffer: Uint8Array): Any | null {
	let v: VarInt;

	[v, buffer] = VarInt.decode(buffer);
	const frameType = v.value;

	// PADDING
	if (frameType === 0x00n) {
		return null;
	}

	// STREAM frames: 0x08-0x0f
	if (frameType >= 0x08n && frameType <= 0x0fn) {
		const hasOff = (frameType & 0x04n) !== 0n;
		const hasLen = (frameType & 0x02n) !== 0n;
		const hasFin = (frameType & 0x01n) !== 0n;

		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		let offset = 0n;
		if (hasOff) {
			[v, buffer] = VarInt.decode(buffer);
			offset = v.value;
		}

		let data: Uint8Array;
		if (hasLen) {
			[v, buffer] = VarInt.decode(buffer);
			const len = Number(v.value);
			[data, buffer] = take(buffer, len);
		} else {
			data = buffer;
		}

		return { type: "stream", id, offset, data, fin: hasFin };
	}

	// RESET_STREAM
	if (frameType === 0x04n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		[v, buffer] = VarInt.decode(buffer);
		const finalSize = v.value;

		return { type: "reset_stream", id, code, finalSize };
	}

	// RESET_STREAM_AT (draft-02). On a reliable, ordered transport all stream data
	// up to final_size has already arrived, so we treat it as a plain reset;
	// reliable_size > final_size is a FRAME_ENCODING_ERROR.
	if (frameType === RESET_STREAM_AT) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);
		[v, buffer] = VarInt.decode(buffer);
		const code = v;
		[v, buffer] = VarInt.decode(buffer);
		const finalSize = v.value;
		[v, buffer] = VarInt.decode(buffer);
		const reliableSize = v.value;
		if (reliableSize > finalSize) {
			throw new Error("RESET_STREAM_AT reliable_size exceeds final_size");
		}
		return { type: "reset_stream", id, code, finalSize, reliableSize };
	}

	// STOP_SENDING
	if (frameType === 0x05n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		return { type: "stop_sending", id, code };
	}

	// CONNECTION_CLOSE / APPLICATION_CLOSE
	if (frameType === 0x1cn || frameType === 0x1dn) {
		[v, buffer] = VarInt.decode(buffer);
		const code = v;

		// The Frame Type field is present only for CONNECTION_CLOSE (0x1c);
		// APPLICATION_CLOSE (0x1d) omits it (RFC 9000 §19.19).
		if (frameType === 0x1cn) {
			[v, buffer] = VarInt.decode(buffer);
		}

		// reason_length + reason
		[v, buffer] = VarInt.decode(buffer);
		const reasonLen = Number(v.value);
		let reasonBytes: Uint8Array;
		[reasonBytes, buffer] = take(buffer, reasonLen);
		const reason = new TextDecoder().decode(reasonBytes);

		return frameType === 0x1dn
			? { type: "application_close", code, reason }
			: { type: "connection_close", code, reason };
	}

	// MAX_DATA
	if (frameType === 0x10n) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "max_data", max: v.value };
	}

	// MAX_STREAM_DATA
	if (frameType === 0x11n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);
		[v, buffer] = VarInt.decode(buffer);
		return { type: "max_stream_data", id, max: v.value };
	}

	// MAX_STREAMS (bidi)
	if (frameType === 0x12n) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "max_streams_bidi", max: v.value };
	}

	// MAX_STREAMS (uni)
	if (frameType === 0x13n) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "max_streams_uni", max: v.value };
	}

	// DATA_BLOCKED
	if (frameType === 0x14n) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "data_blocked", limit: v.value };
	}

	// STREAM_DATA_BLOCKED
	if (frameType === 0x15n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);
		[v, buffer] = VarInt.decode(buffer);
		return { type: "stream_data_blocked", id, limit: v.value };
	}

	// STREAMS_BLOCKED (bidi)
	if (frameType === 0x16n) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "streams_blocked_bidi", limit: v.value };
	}

	// STREAMS_BLOCKED (uni)
	if (frameType === 0x17n) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "streams_blocked_uni", limit: v.value };
	}

	// QX_TRANSPORT_PARAMETERS
	if (frameType === 0x3f5153300d0a0d0an) {
		[v, buffer] = VarInt.decode(buffer);
		const len = Number(v.value);
		let payload: Uint8Array;
		[payload, buffer] = take(buffer, len);
		const params = decodeTransportParams(payload);
		return { type: "transport_parameters", params };
	}

	// QX_PING request
	if (frameType === QX_PING_REQUEST) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "ping_request", sequence: v.value };
	}

	// QX_PING response
	if (frameType === QX_PING_RESPONSE) {
		[v, buffer] = VarInt.decode(buffer);
		return { type: "ping_response", sequence: v.value };
	}

	// DATAGRAM without length — payload runs to the end of the record
	if (frameType === 0x30n) {
		return { type: "datagram", data: buffer, lengthPrefixed: false };
	}

	// DATAGRAM with length
	if (frameType === 0x31n) {
		[v, buffer] = VarInt.decode(buffer);
		const len = Number(v.value);
		let data: Uint8Array;
		[data, buffer] = take(buffer, len);
		return { type: "datagram", data, lengthPrefixed: true };
	}

	throw new Error(`Invalid QMux frame type: 0x${frameType.toString(16)}`);
}

/** Decode a single QMux frame, returning the frame and remaining buffer.
 * Returns null if the buffer is empty.
 */
function decodeQMuxOne(buffer: Uint8Array): [Any | null, Uint8Array] | null {
	if (buffer.byteLength === 0) return null;

	let v: VarInt;
	[v, buffer] = VarInt.decode(buffer);
	const frameType = v.value;

	// PADDING
	if (frameType === 0x00n) {
		return [null, buffer];
	}

	// STREAM frames: 0x08-0x0f
	if (frameType >= 0x08n && frameType <= 0x0fn) {
		const hasOff = (frameType & 0x04n) !== 0n;
		const hasLen = (frameType & 0x02n) !== 0n;
		const hasFin = (frameType & 0x01n) !== 0n;

		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);

		let offset = 0n;
		if (hasOff) {
			[v, buffer] = VarInt.decode(buffer);
			offset = v.value;
		}

		let data: Uint8Array;
		if (hasLen) {
			[v, buffer] = VarInt.decode(buffer);
			const len = Number(v.value);
			[data, buffer] = take(buffer, len);
		} else {
			data = buffer;
			buffer = buffer.slice(buffer.byteLength);
		}

		return [{ type: "stream", id, offset, data, fin: hasFin }, buffer];
	}

	// RESET_STREAM
	if (frameType === 0x04n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);
		[v, buffer] = VarInt.decode(buffer);
		const code = v;
		[v, buffer] = VarInt.decode(buffer);
		const finalSize = v.value;
		return [{ type: "reset_stream", id, code, finalSize }, buffer];
	}

	// RESET_STREAM_AT (draft-02); see the single-frame decoder for the semantics.
	if (frameType === RESET_STREAM_AT) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);
		[v, buffer] = VarInt.decode(buffer);
		const code = v;
		[v, buffer] = VarInt.decode(buffer);
		const finalSize = v.value;
		[v, buffer] = VarInt.decode(buffer);
		const reliableSize = v.value;
		if (reliableSize > finalSize) {
			throw new Error("RESET_STREAM_AT reliable_size exceeds final_size");
		}
		return [{ type: "reset_stream", id, code, finalSize, reliableSize }, buffer];
	}

	// STOP_SENDING
	if (frameType === 0x05n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);
		[v, buffer] = VarInt.decode(buffer);
		const code = v;
		return [{ type: "stop_sending", id, code }, buffer];
	}

	// CONNECTION_CLOSE / APPLICATION_CLOSE
	if (frameType === 0x1cn || frameType === 0x1dn) {
		[v, buffer] = VarInt.decode(buffer);
		const code = v;
		// Frame Type field present only for 0x1c; 0x1d omits it (RFC 9000 §19.19).
		if (frameType === 0x1cn) {
			[v, buffer] = VarInt.decode(buffer); // frame_type
		}
		[v, buffer] = VarInt.decode(buffer);
		const reasonLen = Number(v.value);
		let reasonBytes: Uint8Array;
		[reasonBytes, buffer] = take(buffer, reasonLen);
		const reason = new TextDecoder().decode(reasonBytes);
		const close: Any =
			frameType === 0x1dn
				? { type: "application_close", code, reason }
				: { type: "connection_close", code, reason };
		return [close, buffer];
	}

	// MAX_DATA
	if (frameType === 0x10n) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "max_data", max: v.value }, buffer];
	}

	// MAX_STREAM_DATA
	if (frameType === 0x11n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "max_stream_data", id, max: v.value }, buffer];
	}

	// MAX_STREAMS (bidi)
	if (frameType === 0x12n) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "max_streams_bidi", max: v.value }, buffer];
	}

	// MAX_STREAMS (uni)
	if (frameType === 0x13n) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "max_streams_uni", max: v.value }, buffer];
	}

	// DATA_BLOCKED
	if (frameType === 0x14n) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "data_blocked", limit: v.value }, buffer];
	}

	// STREAM_DATA_BLOCKED
	if (frameType === 0x15n) {
		[v, buffer] = VarInt.decode(buffer);
		const id = new Stream.Id(v);
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "stream_data_blocked", id, limit: v.value }, buffer];
	}

	// STREAMS_BLOCKED (bidi)
	if (frameType === 0x16n) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "streams_blocked_bidi", limit: v.value }, buffer];
	}

	// STREAMS_BLOCKED (uni)
	if (frameType === 0x17n) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "streams_blocked_uni", limit: v.value }, buffer];
	}

	// QX_TRANSPORT_PARAMETERS
	if (frameType === 0x3f5153300d0a0d0an) {
		[v, buffer] = VarInt.decode(buffer);
		const len = Number(v.value);
		let payload: Uint8Array;
		[payload, buffer] = take(buffer, len);
		const params = decodeTransportParams(payload);
		return [{ type: "transport_parameters", params }, buffer];
	}

	// QX_PING request
	if (frameType === QX_PING_REQUEST) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "ping_request", sequence: v.value }, buffer];
	}

	// QX_PING response
	if (frameType === QX_PING_RESPONSE) {
		[v, buffer] = VarInt.decode(buffer);
		return [{ type: "ping_response", sequence: v.value }, buffer];
	}

	// DATAGRAM without length — payload runs to the end of the record
	if (frameType === 0x30n) {
		const data = buffer;
		return [{ type: "datagram", data, lengthPrefixed: false }, buffer.slice(buffer.byteLength)];
	}

	// DATAGRAM with length
	if (frameType === 0x31n) {
		[v, buffer] = VarInt.decode(buffer);
		const len = Number(v.value);
		let data: Uint8Array;
		[data, buffer] = take(buffer, len);
		return [{ type: "datagram", data, lengthPrefixed: true }, buffer];
	}

	throw new Error(`Invalid QMux frame type: 0x${frameType.toString(16)}`);
}
