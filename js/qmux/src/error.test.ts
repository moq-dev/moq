import { expect, test } from "bun:test";
import { resetCode, SessionError, StreamError, streamCode } from "./error.ts";

test("StreamError is shaped like a stream-source WebTransportError", () => {
	const err = new StreamError(26, "RESET_STREAM");
	expect(err.name).toBe("WebTransportError");
	expect(err.source).toBe("stream");
	expect(err.streamErrorCode).toBe(26);
	expect(err.message).toBe("RESET_STREAM: 26");
});

test("StreamError needs only a code, for sending one of your own", () => {
	expect(new StreamError(26).message).toBe("stream reset: 26");
});

test("SessionError has no stream code", () => {
	expect(new SessionError("idle timeout").streamErrorCode).toBeNull();
});

test("resetCode: reads the code off a WebTransportError-shaped reason", () => {
	expect(resetCode(new StreamError(2, "RESET_STREAM"))).toBe(2);
	expect(resetCode({ streamErrorCode: 31 })).toBe(31);
});

test("resetCode: anything without a code resets with 0", () => {
	expect(resetCode(new Error("cancel"))).toBe(0);
	expect(resetCode(undefined)).toBe(0);
	expect(resetCode("nope")).toBe(0);
	expect(resetCode({ streamErrorCode: null })).toBe(0);
	expect(resetCode({ streamErrorCode: Number.NaN })).toBe(0);
});

test("resetCode: wraps to unsigned 32-bit like the IDL type", () => {
	expect(resetCode({ streamErrorCode: -1 })).toBe(0xffffffff);
	expect(resetCode({ streamErrorCode: 2 ** 32 })).toBe(0);
});

test("streamCode: narrows a varint off the wire to an unsigned 32-bit code", () => {
	expect(streamCode(26n)).toBe(26);
	// A peer can encode a code far wider than the IDL type, and wider than Number holds
	// exactly. Truncating keeps streamErrorCode a value the WebTransport API can represent.
	expect(streamCode((1n << 62n) - 1n)).toBe(0xffffffff);
	expect(streamCode(1n << 32n)).toBe(0);
});
