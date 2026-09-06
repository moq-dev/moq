import { afterEach, beforeEach, expect, test } from "bun:test";
import { StreamError as QmuxStreamError } from "@moq/qmux";
import {
	FrameTooLarge,
	fromClose,
	fromTransport,
	Lagged,
	NotFound,
	ProtocolViolation,
	reason,
	SessionCode,
	SessionError,
	StreamCode,
	StreamError,
	toStreamCode,
	toTransport,
} from "./error.ts";
import { TimeoutError } from "./util/timeout.ts";

// Stand-in for the current WebTransportError constructor, which the test runtime may not define.
class FakeWebTransportError extends Error {
	readonly source: string;
	readonly streamErrorCode: number | null;

	constructor(message?: string, options?: { source?: string; streamErrorCode?: number | null }) {
		super(message ?? "");
		this.name = "WebTransportError";
		this.source = options?.source ?? "stream";
		this.streamErrorCode = options?.streamErrorCode ?? null;
	}
}

// Chromium still implements the previous constructor shape even though the current DOM types
// expose `(message, options)`. This strict stand-in reproduces Chromium rejecting a string where
// it expects WebTransportErrorInit.
class LegacyFakeWebTransportError extends Error {
	static standardAttempts = 0;

	readonly source = "stream";
	readonly streamErrorCode: number | null;

	constructor(init: { message?: string; streamErrorCode?: number | null }) {
		if (typeof init !== "object" || init === null) {
			LegacyFakeWebTransportError.standardAttempts += 1;
			throw new TypeError("The provided value is not of type 'WebTransportErrorInit'");
		}
		super(init.message ?? "");
		this.name = "WebTransportError";
		this.streamErrorCode = init.streamErrorCode ?? null;
	}
}

/** The old positional shape, kept so the existing cases stay readable. */
function fake(source: string, streamErrorCode: number | null, message = ""): FakeWebTransportError {
	return new FakeWebTransportError(message, { source, streamErrorCode });
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
	const src = fake("stream", 2);
	const err = fromTransport(src);
	expect(err).toBeInstanceOf(StreamError);
	expect((err as StreamError).code).toBe(StreamCode.DeliveryTimeout);
	expect(err.message).toBe("remote error: 2");
	// The original error stays reachable for logging.
	expect(err.cause).toBe(src);
});

// Both registries assign 2, to UNAUTHORIZED and DELIVERY_TIMEOUT respectively. Distinct
// classes and branded code types keep callers from reading either against the wrong table.
test("the error type names the registry the code came from", () => {
	const stream = fromTransport(fake("stream", 2)) as StreamError;
	const session = fromClose({ closeCode: 2, reason: "unauthorized" });

	expect(stream).toBeInstanceOf(StreamError);
	expect(session).toBeInstanceOf(SessionError);
	expect(Number(stream.code)).toBe(Number(session?.code));
});

test("the code types reject the other registry", () => {
	new StreamError(StreamCode.DeliveryTimeout);
	new SessionError(SessionCode.Unauthorized);
	new StreamError(StreamCode(64));
	new SessionError(SessionCode(64));

	// @ts-expect-error A session error cannot carry a stream code.
	new SessionError(StreamCode.DeliveryTimeout);
	// @ts-expect-error A stream error cannot carry a session code.
	new StreamError(SessionCode.Unauthorized);
	// @ts-expect-error Raw numbers have not been decoded against either registry.
	new StreamError(2);

	const mutateRegistry = () => {
		// @ts-expect-error Registry constants are readonly.
		SessionCode.Cancel = SessionCode(64);
		// @ts-expect-error Registry constants are readonly.
		StreamCode.Cancel = StreamCode(64);
	};
	expect(mutateRegistry).toBeDefined();
});

test("application code constructors validate the application range", () => {
	expect(Number(StreamCode(64))).toBe(64);
	expect(Number(SessionCode(0xffffffff))).toBe(0xffffffff);

	for (const code of [63, 1.5, -1, 0x100000000, Number.NaN]) {
		expect(() => StreamCode(code)).toThrow(RangeError);
		expect(() => SessionCode(code)).toThrow(RangeError);
	}
});

test("code registries cannot be mutated at runtime", () => {
	expect(Object.isFrozen(SessionCode)).toBe(true);
	expect(Object.isFrozen(StreamCode)).toBe(true);
	expect(() => Object.assign(SessionCode, { Cancel: SessionCode(64) })).toThrow(TypeError);
	expect(() => Object.assign(StreamCode, { Cancel: StreamCode(64) })).toThrow(TypeError);
});

test("fromTransport: code 0 is a code like any other", () => {
	// 0 is what a transport sends for a stream dropped with no code of its own. What it
	// means is up to the peer, so it gets no special treatment here.
	expect((fromTransport(fake("stream", 0)) as StreamError).code).toBe(StreamCode.Internal);
});

test("fromTransport: decodes a fallback error with no WebTransportError global", () => {
	// The WebSocket fallback runs where the browser has no WebTransport, so the decode must
	// read the fields rather than the class.
	globals.WebTransportError = undefined;
	const err = fromTransport({ source: "stream", streamErrorCode: 31, message: "" });
	expect(err).toBeInstanceOf(StreamError);
	expect(Number((err as StreamError).code)).toBe(31);
});

test("fromTransport: a session failure has no stream code, so it passes through", () => {
	const src = fake("session", null, "connection lost");
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
	expect(reason(fake("stream", 0))).toBe("WebTransportError: source=stream code=0");
});

test("reason: WebTransportError omits a null stream error code", () => {
	expect(reason(fake("session", null))).toBe("WebTransportError: source=session");
});

test("reason: WebTransportError keeps a populated message and appends details", () => {
	expect(reason(fake("stream", 42, "Received RESET_STREAM."))).toBe("Received RESET_STREAM. (source=stream code=42)");
});

test("reason: a decoded remote error names its code", () => {
	expect(reason(new StreamError(StreamCode.TooFarBehind))).toBe("remote error: 5");
});

test("fromClose: a clean close is null, a coded close keeps its code", () => {
	expect(fromClose({ closeCode: SessionCode.Cancel, reason: "" })).toBeNull();
	// A missing code is what a transport reports for a close with no code of its own.
	expect(fromClose({})).toBeNull();

	const err = fromClose({ closeCode: SessionCode.Unauthorized, reason: "unauthorized" });
	expect(err).toBeInstanceOf(SessionError);
	expect(err?.code).toBe(SessionCode.Unauthorized);
	expect(err?.message).toBe("remote error: 2 (unauthorized)");

	expect(fromClose({ closeCode: SessionCode.GoawayTimeout })?.message).toBe("remote error: 16");
});

// The two registries are wire contracts (draft-lcurley-moq-lite, Error Codes) and must match
// the Rust tables, since the two implementations talk to each other.
test("the code tables match the spec", () => {
	// moq-transport's, reused unchanged.
	expect(Number(SessionCode.Cancel)).toBe(0x0);
	expect(Number(SessionCode.Unauthorized)).toBe(0x2);
	expect(Number(SessionCode.GoawayTimeout)).toBe(0x10);
	expect(Number(SessionCode.Version)).toBe(0x15);
	expect(Number(StreamCode.Cancel)).toBe(0x1);
	expect(Number(StreamCode.SessionClosed)).toBe(0x3);
	expect(Number(StreamCode.TooFarBehind)).toBe(0x5);

	// The session table carries only what the draft assigns.
	for (const code of Object.values(SessionCode)) {
		expect(code).toBeLessThan(32);
	}

	// The stream table adds the reserved-range placeholders this implementation sends, and
	// nothing else lands there: 32-63 carries no meaning the draft publishes, so a code we
	// put in it is an agreement with our own Rust implementation rather than a spec value.
	const placeholders: StreamCode[] = [
		StreamCode.NotFound,
		StreamCode.Old,
		StreamCode.Evicted,
		StreamCode.FrameTooLarge,
	];
	for (const code of Object.values(StreamCode)) {
		if (placeholders.includes(code)) {
			expect(code).toBeGreaterThanOrEqual(32);
			expect(code).toBeLessThan(64);
		} else {
			expect(code).toBeLessThan(32);
		}
	}
	// The values the Rust `StreamError` sends for the same conditions.
	expect(Number(StreamCode.NotFound)).toBe(0x20);
	expect(Number(StreamCode.Old)).toBe(0x22);
	expect(Number(StreamCode.Evicted)).toBe(0x23);
	expect(Number(StreamCode.FrameTooLarge)).toBe(0x25);

	// The spaces are disjoint: 0 ends a session cleanly but fails a stream.
	expect(Number(SessionCode.Cancel)).not.toBe(Number(StreamCode.Cancel));
	expect(Number(StreamCode.Internal)).toBe(0x0);
});

test("fromTransport: decodes a real qmux stream reset", () => {
	// The WebSocket fallback's actual error type, not a stand-in: this is the contract the
	// fallback path depends on, and it only holds from @moq/qmux 0.3.2 on. Before that, qmux
	// formatted the code into an Error message and this silently decoded nothing.
	const err = fromTransport(new QmuxStreamError(2, "RESET_STREAM"));
	expect(err).toBeInstanceOf(StreamError);
	expect((err as StreamError).code).toBe(StreamCode.DeliveryTimeout);
});

// The transports read the reset code off a WebTransportError's `streamErrorCode` and send 0
// for anything else. 0 is INTERNAL_ERROR, so cancelling with a bare Error tells the peer we
// failed: every routine unsubscribe would land as a publisher-side error rather than a cancel.
test("toTransport: carries a code the transports will actually send", () => {
	const reason = toTransport(StreamCode.Cancel, "cancel");
	expect((reason as unknown as { source: string }).source).toBe("stream");
	expect((reason as unknown as { streamErrorCode: number }).streamErrorCode).toBe(StreamCode.Cancel);

	// A plain Error carries nothing, which is what makes the mistake silent.
	expect(fromTransport(new Error("cancel"))).not.toBeInstanceOf(StreamError);

	// Round trip: what we send is what a peer decodes.
	const decoded = fromTransport(reason);
	expect(decoded).toBeInstanceOf(StreamError);
	expect((decoded as StreamError).code).toBe(StreamCode.Cancel);
	expect((decoded as StreamError).code).not.toBe(StreamCode.Internal);
});

test("toTransport: supports Chromium's single-dictionary constructor", () => {
	globals.WebTransportError = LegacyFakeWebTransportError;
	const reason = toTransport(StreamCode.Cancel, "cancel");
	expect(reason).toBeInstanceOf(LegacyFakeWebTransportError);
	expect(reason.message).toBe("cancel");
	expect((reason as LegacyFakeWebTransportError).streamErrorCode).toBe(StreamCode.Cancel);

	// Remember the constructor shape so routine stream cancellation does not throw every time.
	toTransport(StreamCode.Cancel, "cancel again");
	expect(LegacyFakeWebTransportError.standardAttempts).toBe(1);
});

// Without the global, the fallback must still produce the shape qmux and fromTransport read.
test("toTransport: works with no WebTransportError global", () => {
	globals.WebTransportError = undefined;
	const decoded = fromTransport(toTransport(StreamCode.TooFarBehind, "lagged"));
	expect((decoded as StreamError).code).toBe(StreamCode.TooFarBehind);
});

// The whole point of the pair: a condition raised here goes out as its own code, and comes back
// at the peer as the same condition. Without it every local error is 0 (INTERNAL_ERROR), so a
// lagging reader or an unknown broadcast reads as a crash on the sender's side.
test("toStreamCode: a local condition maps to the code the peer can act on", () => {
	expect(toStreamCode(new Lagged())).toBe(StreamCode.TooFarBehind);
	expect(toStreamCode(new FrameTooLarge())).toBe(StreamCode.FrameTooLarge);
	expect(toStreamCode(new NotFound("broadcast x"))).toBe(StreamCode.NotFound);
	expect(toStreamCode(new TimeoutError("too slow"))).toBe(StreamCode.DeliveryTimeout);
	// Session-scoped: the peer learns which rule it broke from the session close.
	expect(toStreamCode(new ProtocolViolation("bad hop"))).toBe(StreamCode.SessionClosed);

	// A peer's code is forwarded verbatim, which is what keeps a relay from flattening it.
	expect(toStreamCode(new StreamError(StreamCode.GoingAway))).toBe(StreamCode.GoingAway);
	expect(toStreamCode(new StreamError(StreamCode(70)))).toBe(StreamCode(70));

	// Anything else already means 0, and so does a session code: the registries are disjoint, so
	// forwarding one onto a stream would mistranslate it (session 0x4 is a stream GOING_AWAY).
	expect(toStreamCode(new Error("boom"))).toBe(StreamCode.Internal);
	expect(toStreamCode(new SessionError(SessionCode.GoawayTimeout))).toBe(StreamCode.Internal);
});

test("toStreamCode and fromTransport agree on what a code means", () => {
	// Every registered code survives the round trip, so a relay decoding and re-encoding one
	// cannot change what the next hop reads.
	for (const code of [
		StreamCode.Internal,
		StreamCode.Cancel,
		StreamCode.DeliveryTimeout,
		StreamCode.SessionClosed,
		StreamCode.GoingAway,
		StreamCode.TooFarBehind,
		StreamCode.MalformedTrack,
		StreamCode(70),
	]) {
		expect(toStreamCode(fromTransport(toTransport(code, "reset")))).toBe(code);
	}

	// A gap is one class whichever side it happened on, so a `catch` needs only one check.
	expect(fromTransport(toTransport(StreamCode.TooFarBehind, "lagged"))).toBeInstanceOf(Lagged);
	expect(new Lagged()).toBeInstanceOf(StreamError);
});
