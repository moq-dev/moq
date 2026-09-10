import { expect, test } from "bun:test";
import { SessionCode, SessionError, StreamCode, StreamError } from "@moq/net";
import { Once } from "@moq/signals";
import { isCleanEnd, isDecoderEnd } from "./error";

test("isCleanEnd is finish or Cancel, not a message match", () => {
	expect(isCleanEnd(null)).toBe(true);
	expect(isCleanEnd(new StreamError(StreamCode.Cancel))).toBe(true);
	expect(isCleanEnd(undefined)).toBe(false);
	expect(isCleanEnd(new Error("cancel"))).toBe(false);
	expect(isCleanEnd(new StreamError(StreamCode.Internal, { message: "cancel" }))).toBe(false);
	expect(isCleanEnd(new SessionError(SessionCode.Cancel))).toBe(false);
});

test("isDecoderEnd classifies from the stream's end, not the exception", () => {
	const closed = new Once<Error | null>();
	const consumer = { cancelled: false, closed };

	expect(isDecoderEnd(consumer, false)).toBe(false);
	expect(isDecoderEnd(consumer, true)).toBe(true);

	consumer.cancelled = true;
	expect(isDecoderEnd(consumer, false)).toBe(true);

	consumer.cancelled = false;
	closed.set(new StreamError(StreamCode.Cancel));
	expect(isDecoderEnd(consumer, false)).toBe(true);
});

test("isDecoderEnd treats a clean finish as a requested end", () => {
	const closed = new Once<Error | null>();
	closed.set(null);
	expect(isDecoderEnd({ cancelled: false, closed }, false)).toBe(true);
});

test("isDecoderEnd treats any other abort as a fault", () => {
	const closed = new Once<Error | null>();
	closed.set(new StreamError(StreamCode.Internal));
	expect(isDecoderEnd({ cancelled: false, closed }, false)).toBe(false);
});
