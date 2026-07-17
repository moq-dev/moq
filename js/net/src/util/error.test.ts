import { afterEach, beforeEach, expect, test } from "bun:test";
import { reason } from "./error.ts";

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
