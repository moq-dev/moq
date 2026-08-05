import { afterEach, beforeEach, expect, test } from "bun:test";
import { StreamError } from "@moq/qmux";
import { fromClose, fromTransport, RemoteError, reason, SessionCode, StreamCode } from "./error.ts";

// Minimal stand-in for the DOM WebTransportError, which the test runtime may not define.
class FakeWebTransportError extends Error {
	readonly source: string;
	readonly streamErrorCode: number | null;

	constructor(source: string, streamErrorCode: number | null, message = "") {
		super(message);
		this.name = "WebTransportError";
		this.source = source;
		this.streamErrorCode = streamErrorCode;
	}
}

const globals = globalThis as { WebTransportError?: unknown };
let originalWebTransportError: unknown;

beforeEach(() => {
	// Force `instanceof WebTransportError` in reason() to match our stand-in.
	originalWebTransportError = globals.WebTransportError;
	globals.WebTransportError = FakeWebTransportError;
});

afterEach(() => {
	globals.WebTransportError = originalWebTransportError;
});

test("fromTransport: a stream reset keeps the peer's code verbatim", () => {
	const src = new FakeWebTransportError("stream", 2);
	const err = fromTransport(src);
	expect(err).toBeInstanceOf(RemoteError);
	expect((err as RemoteError).code).toBe(2);
	expect(err.message).toBe("remote error: 2");
	// The original error stays reachable for logging.
	expect(err.cause).toBe(src);
});

test("fromTransport: code 0 is a code like any other", () => {
	// 0 is what a transport sends for a stream dropped with no code of its own. What it
	// means is up to the peer, so it gets no special treatment here.
	expect((fromTransport(new FakeWebTransportError("stream", 0)) as RemoteError).code).toBe(0);
});

test("fromTransport: decodes a fallback error with no WebTransportError global", () => {
	// The WebSocket fallback runs where the browser has no WebTransport, so the decode must
	// read the fields rather than the class.
	globals.WebTransportError = undefined;
	const err = fromTransport({ source: "stream", streamErrorCode: 31, message: "" });
	expect(err).toBeInstanceOf(RemoteError);
	expect((err as RemoteError).code).toBe(31);
});

test("fromTransport: a session failure has no stream code, so it passes through", () => {
	const src = new FakeWebTransportError("session", null, "connection lost");
	expect(fromTransport(src)).toBe(src);
});

test("fromTransport: a local error passes through and a non-error is coerced", () => {
	// Our own messages are better than a code, so they survive untouched.
	const src = new Error("unexpected end of stream");
	expect(fromTransport(src)).toBe(src);
	expect(fromTransport("nope").message).toBe("nope");
});

test("reason: plain Error keeps its message", () => {
	expect(reason(new Error("boom"))).toBe("boom");
});

test("reason: non-Error value is stringified", () => {
	expect(reason("nope")).toBe("nope");
});

test("reason: empty message falls back to the type name", () => {
	// Safari's WebTransportError.message is always blank; the reason must never be empty.
	expect(reason(new Error(""))).toBe("Error");
});

test("reason: WebTransportError with a blank message surfaces source and code", () => {
	// The Safari case from the bug report: a RESET_STREAM with no message.
	expect(reason(new FakeWebTransportError("stream", 0))).toBe("WebTransportError: source=stream code=0");
});

test("reason: WebTransportError omits a null stream error code", () => {
	expect(reason(new FakeWebTransportError("session", null))).toBe("WebTransportError: source=session");
});

test("reason: WebTransportError keeps a populated message and appends details", () => {
	expect(reason(new FakeWebTransportError("stream", 42, "Received RESET_STREAM."))).toBe(
		"Received RESET_STREAM. (source=stream code=42)",
	);
});

test("reason: a decoded remote error names its code", () => {
	expect(reason(new RemoteError(31))).toBe("remote error: 31");
});

test("fromClose: a clean close is null, a coded close keeps its code", () => {
	expect(fromClose({ closeCode: SessionCode.Cancel, reason: "" })).toBeNull();
	// A missing code is what a transport reports for a close with no code of its own.
	expect(fromClose({})).toBeNull();

	const err = fromClose({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" });
	expect(err).toBeInstanceOf(RemoteError);
	expect(err?.code).toBe(0x2);
	expect(err?.message).toBe("remote error: 2 (unauthorized)");

	expect(fromClose({ closeCode: SessionCode.GoawayTimeout })?.message).toBe("remote error: 16");
});

// The two registries are wire contracts (draft-lcurley-moq-lite, Error Codes) and must match
// the Rust tables, since the two implementations talk to each other.
test("the code tables match the spec", () => {
	// moq-transport's, reused unchanged.
	expect(SessionCode.Cancel).toBe(0x0);
	expect(SessionCode.Unauthorized).toBe(0x2);
	expect(SessionCode.GoawayTimeout).toBe(0x10);
	expect(SessionCode.Version).toBe(0x15);
	expect(StreamCode.Cancel).toBe(0x1);
	expect(StreamCode.SessionClosed).toBe(0x3);
	expect(StreamCode.TooFarBehind).toBe(0x5);

	// The tables carry only what the draft assigns. 32-63 is reserved, so a code there is
	// something an implementation happens to send, not a meaning to publish.
	for (const code of [...Object.values(SessionCode), ...Object.values(StreamCode)]) {
		expect(code).toBeLessThan(32);
	}

	// The spaces are disjoint: 0 ends a session cleanly but fails a stream.
	expect(SessionCode.Cancel).not.toBe(StreamCode.Cancel);
	expect(StreamCode.Internal).toBe(0x0);
});

test("fromTransport: decodes a real qmux stream reset", () => {
	// The WebSocket fallback's actual error type, not a stand-in: this is the contract the
	// fallback path depends on, and it only holds from @moq/qmux 0.3.2 on. Before that, qmux
	// formatted the code into an Error message and this silently decoded nothing.
	const err = fromTransport(new StreamError(2, "RESET_STREAM"));
	expect(err).toBeInstanceOf(RemoteError);
	expect((err as RemoteError).code).toBe(2);
});
