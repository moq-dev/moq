import { expect, test } from "bun:test";
import { isStreamAbort } from "./error.ts";

// A stand-in for WebTransportError, which the test runner does not define. isStreamAbort keys on
// `err.name` + duck-typed `source`/`streamErrorCode`, so this reproduces what the browser passes.
function wtError(source: string, streamErrorCode?: number | null): Error {
	const err = new Error("The stream was aborted by the remote server.");
	err.name = "WebTransportError";
	return Object.assign(err, { source, streamErrorCode });
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

test("non-stream-reset errors are surfaced", () => {
	expect(isStreamAbort(new Error("first subscribe response must be SUBSCRIBE_OK"))).toBe(false);
	expect(isStreamAbort(wtError("session", 0))).toBe(false); // session-level, not a stream reset
	expect(isStreamAbort("not an error")).toBe(false);
	expect(isStreamAbort(undefined)).toBe(false);
});
