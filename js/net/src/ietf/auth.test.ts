import { expect, test } from "bun:test";
import { Unsupported } from "../auth.ts";
import { SessionError } from "../error.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { AuthError, AuthMessage, AuthOk, fromSetup, intoSetup } from "./auth.ts";
import { SetupOption, SetupOptions } from "./parameters.ts";
import { Version } from "./version.ts";

function patterns(...prefixes: string[]): Path.Patterns {
	return new Path.Patterns(prefixes.map((prefix) => Path.Pattern.subtree(prefix)));
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

test("AUTH_OK carries prefixes as namespace tuples", async () => {
	const ok = new AuthOk(patterns("room/alice"), patterns(), undefined);
	const wire = await bytes((w) => ok.encode(w, Version.DRAFT_17));
	// Type, 16-bit length, then: one prefix of two fields, no subscribe prefixes, never expires.
	expect([...wire.slice(-15)]).toEqual([
		0x01,
		0x02,
		0x04,
		...new TextEncoder().encode("room"),
		0x05,
		...new TextEncoder().encode("alice"),
		0x00,
		0x00,
	]);

	const r = await afterType((w) => new AuthOk(patterns(""), patterns("room"), 60_000).encode(w, Version.DRAFT_17));
	const got = await AuthOk.decode(r, Version.DRAFT_17);
	// The empty prefix grants everything; the empty list grants nothing.
	expect(got.publish.equals(new Path.Patterns([Path.Pattern.all()]))).toBe(true);
	expect(got.subscribe.equals(patterns("room"))).toBe(true);
	expect(got.expires).toBe(60_000);
});

test("a grant the prefix wire cannot express is refused before anything is written", async () => {
	for (const union of [["room/alice"], ["room/*/cam"], ["room/**", "lobby"]]) {
		const narrow = new AuthOk(new Path.Patterns(union.map((p) => Path.Pattern.parse(p))), patterns());
		let written = 0;
		const writer = new Writer(
			new WritableStream({
				write(chunk) {
					written += chunk.byteLength;
				},
			}),
			Version.DRAFT_17,
		);
		await expect(narrow.encode(writer, Version.DRAFT_17)).rejects.toBeInstanceOf(Unsupported);
		writer.close();
		await writer.closed;
		expect(written).toBe(0);
	}
});

test("a grant too large for one message is refused before anything is written", async () => {
	const huge = new AuthOk(patterns(...Array.from({ length: 20 }, (_, i) => `${i}${"x".repeat(4000)}`)), patterns());
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

test("NOT_SUPPORTED is unsupported, every other code a refusal", async () => {
	const r = await afterType((w) => new AuthError(0x3, "prefixes only").encode(w, Version.DRAFT_17));
	expect((await AuthError.decode(r, Version.DRAFT_17)).toError()).toBeInstanceOf(Unsupported);
	expect(new AuthError(0x1, "bad").toError()).toBeInstanceOf(SessionError);
});

test("drafts before 17 carry no AUTH", async () => {
	await expect(bytes((w) => new AuthMessage(0n, new Uint8Array()).encode(w, Version.DRAFT_16))).rejects.toThrow();
});
