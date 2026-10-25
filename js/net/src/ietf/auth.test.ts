import { expect, test } from "bun:test";
import { Unsupported } from "../auth.ts";
import { SessionCode, type SessionError, StreamCode, type StreamError } from "../error.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { AuthError, AuthMessage, AuthOk } from "./auth.ts";
import { fromSetup, intoSetup } from "./auth_setup.ts";
import { SetupOption, SetupOptions } from "./setup_options.ts";
import { Version } from "./version.ts";

function patterns(...texts: string[]): Path.Patterns {
	return new Path.Patterns(texts.map((text) => Path.Pattern.parse(text)));
}

/** The bytes a write produces. */
async function bytes(write: (w: Writer) => Promise<void>): Promise<Uint8Array> {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream({
			write(chunk) {
				chunks.push(new Uint8Array(chunk));
			},
		}),
		Version.DRAFT_17,
	);
	await write(writer);
	writer.close();
	await writer.closed;
	const out = new Uint8Array(chunks.reduce((n, c) => n + c.byteLength, 0));
	let offset = 0;
	for (const chunk of chunks) {
		out.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return out;
}

/** A reader past the message type, the way the dispatcher leaves one. */
async function afterType(write: (w: Writer) => Promise<void>): Promise<Reader> {
	const reader = new Reader(undefined, await bytes(write), Version.DRAFT_17);
	await reader.u53();
	return reader;
}

test("the setup option negotiates on draft-17+ only", () => {
	for (const version of [Version.DRAFT_17, Version.DRAFT_22]) {
		const params = new SetupOptions();
		expect(fromSetup(params, version)).toBeUndefined();
		intoSetup(params, version);
		expect(fromSetup(params, version)).toBe(true);
	}
	const legacy = new SetupOptions();
	intoSetup(legacy, Version.DRAFT_16);
	expect(legacy.getVarint(SetupOption.Auth)).toBeUndefined();

	// An explicit value other than 1 is an implementation that declined.
	const declined = new SetupOptions();
	declined.setVarint(SetupOption.Auth, 0n);
	expect(fromSetup(declined, Version.DRAFT_17)).toBe(false);
});

test("AUTH round-trips its request id and token", async () => {
	const r = await afterType((w) => new AuthMessage(4n, new TextEncoder().encode("jwt")).encode(w, Version.DRAFT_17));
	const msg = await AuthMessage.decode(r, Version.DRAFT_17);
	expect(msg.requestId).toBe(4n);
	expect(new TextDecoder().decode(msg.token)).toBe("jwt");
});

test("AUTH_OK carries each pattern as its canonical text", async () => {
	// The exact bytes `rs/moq-net/src/ietf/auth.rs` checks, so both encoders agree.
	const ok = new AuthOk(patterns("room/*/cam", "ch1"), patterns(""), undefined);
	const wire = await bytes((w) => ok.encode(w, Version.DRAFT_17));
	// Type, 16-bit length, then: two publish patterns in canonical order, the empty
	// subscribe pattern, never expires.
	expect([...wire.slice(-19)]).toEqual([
		0x02,
		0x03,
		...new TextEncoder().encode("ch1"),
		0x0a,
		...new TextEncoder().encode("room/*/cam"),
		0x01,
		0x00,
		0x00,
	]);

	// Literals and wildcards travel exactly, never widened to a prefix.
	const grant = new AuthOk(patterns("**"), patterns("live/*", "lobby/cam-*.hang", "ch1"), 60_000);
	const r = await afterType((w) => grant.encode(w, Version.DRAFT_17));
	const got = await AuthOk.decode(r, Version.DRAFT_17);
	expect(got.publish.equals(grant.publish)).toBe(true);
	expect(got.subscribe.equals(grant.subscribe)).toBe(true);
	expect(got.expires).toBe(60_000);
});

test("a grant too large for one message is refused before anything is written", async () => {
	const huge = new AuthOk(
		patterns(...Array.from({ length: 20 }, (_, i) => `${i}${"x".repeat(4000)}/**`)),
		patterns(),
	);
	let written = 0;
	const writer = new Writer(
		new WritableStream({
			write(chunk) {
				written += chunk.byteLength;
			},
		}),
		Version.DRAFT_17,
	);
	await expect(huge.encode(writer, Version.DRAFT_17)).rejects.toThrow("Message too large");
	writer.close();
	await writer.closed;
	expect(written).toBe(0);
});

test("AUTH_ERROR preserves actionable refusal kinds", async () => {
	const r = await afterType((w) => new AuthError(0x3, "too large").encode(w, Version.DRAFT_17));
	expect((await AuthError.decode(r, Version.DRAFT_17)).toError()).toBeInstanceOf(Unsupported);
	const expired = new AuthError(0x5, "expired").toError() as SessionError;
	expect(expired.code).toBe(SessionCode.Expired);
	const malformed = new AuthError(0x4, "not a JWT").toError() as SessionError;
	expect(malformed.code).toBe(SessionCode.MalformedAuthToken);
	const unauthorized = new AuthError(0x1, "bad").toError() as SessionError;
	expect(unauthorized.code).toBe(SessionCode.Unauthorized);
	// Past the acceptor's GOAWAY: the request is refused, not the token.
	const goingAway = new AuthError(0x6, "going away").toError() as StreamError;
	expect(goingAway.code).toBe(StreamCode.GoingAway);
});

test("drafts before 17 carry no AUTH", async () => {
	await expect(bytes((w) => new AuthMessage(0n, new Uint8Array()).encode(w, Version.DRAFT_16))).rejects.toThrow();
});
