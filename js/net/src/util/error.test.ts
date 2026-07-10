import { expect, test } from "bun:test";
import { isStreamAbort } from "./error.ts";

// A stand-in for WebTransportError, which the test runner does not define. isStreamAbort keys on
// `err.name` + duck-typed `source`/`streamErrorCode`, so this reproduces what the browser passes.
function wtError(source: string, streamErrorCode?: number | null): Error {
	const err = new Error("The stream was aborted by the remote server.");
	err.name = "WebTransportError";
	return Object.assign(err, { source, streamErrorCode });
}

// A stand-in for the DOMException Safari throws when a stream is used after it ended.
function invalidState(message: string): Error {
	const err = new Error(message);
	err.name = "InvalidStateError";
	return err;
}

test("routine stream resets (Cancel/Dropped/Closed/no-code) are teardown", () => {
	// Native WebTransport (streamErrorCode carries the relay's rs/moq-net error.rs code).
	expect(isStreamAbort(wtError("stream", 0))).toBe(true); // Cancel: normal unsubscribe
	expect(isStreamAbort(wtError("stream", 24))).toBe(true); // Dropped
	expect(isStreamAbort(wtError("stream", 25))).toBe(true); // Closed
	expect(isStreamAbort(wtError("stream", null))).toBe(true); // reset with no app code -> routine
	// WebSocket/qmux fallback ("RESET_STREAM: <code>" / "STOP_SENDING: <code>").
	expect(isStreamAbort(new Error("RESET_STREAM: 0"))).toBe(true);
	expect(isStreamAbort(new Error("STOP_SENDING: 0"))).toBe(true);
});

test("client-actionable fault codes surface (warn), not teardown", () => {
	expect(isStreamAbort(wtError("stream", 6))).toBe(false); // Unauthorized
	expect(isStreamAbort(wtError("stream", 13))).toBe(false); // NotFound (wrong path)
	expect(isStreamAbort(wtError("stream", 15))).toBe(false); // ProtocolViolation
	expect(isStreamAbort(wtError("stream", 30))).toBe(false); // Unroutable
	expect(isStreamAbort(new Error("RESET_STREAM: 13"))).toBe(false); // NotFound over qmux
	expect(isStreamAbort(new Error("STOP_SENDING: 6"))).toBe(false); // Unauthorized over qmux
});

test("write-after-close over the WebSocket/qmux fallback is teardown", () => {
	// Generic Streams-API errors from writing after the stream ended (a peer reset racing an in-flight
	// write). Chromium/Firefox and Safari word it differently; both are routine teardown, not faults.
	expect(isStreamAbort(new Error("The stream is closed or closing"))).toBe(true);
	expect(isStreamAbort(invalidState("The object is in an invalid state."))).toBe(true);
});

test("a session close is teardown, unless the peer signalled a fault", () => {
	// The exact strings qmux constructs when the session ends (see @moq/qmux session.js).
	expect(isStreamAbort(new Error("Connection closed"))).toBe(true); // local close()
	expect(isStreamAbort(new Error("Connection closed: 0: "))).toBe(true); // peer CONNECTION_CLOSE, no error
	expect(isStreamAbort(new Error("Connection closed: 1006 "))).toBe(true); // abnormal WebSocket closure

	// A client-actionable code on the CONNECTION_CLOSE frame still surfaces.
	expect(isStreamAbort(new Error("Connection closed: 6: unauthorized"))).toBe(false); // Unauthorized
	expect(isStreamAbort(new Error("Connection closed: 15: bad frame"))).toBe(false); // ProtocolViolation
});

test("a coded fault wins over the teardown message heuristics", () => {
	// A fault code must not be reclassified as teardown just because the browser's prose happens to
	// mention a closing stream.
	const err = wtError("stream", 6); // Unauthorized
	err.message = "The stream is closed or closing";
	expect(isStreamAbort(err)).toBe(false);
});

test("an unrelated error mentioning an invalid state still surfaces", () => {
	// Keyed on `name`, so one of our own ordering bugs is not silently downgraded to debug.
	expect(isStreamAbort(new Error("decoder is in an invalid state"))).toBe(false);
});

test("non-WebTransport errors are surfaced", () => {
	expect(isStreamAbort(new Error("first subscribe response must be SUBSCRIBE_OK"))).toBe(false);
	expect(isStreamAbort("not an error")).toBe(false);
	expect(isStreamAbort(undefined)).toBe(false);
});

test("WebTransport errors are classified by code regardless of source", () => {
	// Chrome surfaces a write-side abort (a downstream unsubscribe seen by the publisher) as a
	// WebTransportError with source "session" and the relay's Cancel(0) code: routine, must not warn.
	expect(isStreamAbort(wtError("session", 0))).toBe(true);
	expect(isStreamAbort(wtError("session", null))).toBe(true);
	// A genuine session-level fault code still surfaces.
	expect(isStreamAbort(wtError("session", 6))).toBe(false); // Unauthorized
});
