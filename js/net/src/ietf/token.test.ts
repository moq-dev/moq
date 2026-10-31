import { expect, test } from "bun:test";
import { ProtocolViolation, SessionCode } from "../error.ts";
import { Reader, Writer } from "../stream.ts";
import * as Varint from "../varint.ts";
import { Parameters } from "./parameters.ts";
import { SetupOption, SetupOptions } from "./setup_options.ts";
import {
	AUTHORIZATION_TOKEN,
	TOKEN_OUT_OF_BAND,
	type Token,
	tokenFromRequest,
	tokenFromSetup,
	tokenIntoRequest,
	tokenIntoSetup,
} from "./token.ts";
import { type IetfVersion, Version } from "./version.ts";

const VERSIONS: IetfVersion[] = [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22];

// A kind past one varint byte and a value that is not text, matching the Rust tests.
const TOKEN: Token = { kind: 300n, value: new Uint8Array([0x00, 0xff, 0x03, 0x80, 0x6a]) };

/** The option as it arrives, after a trip through the SETUP parameter block. */
async function received(params: SetupOptions, version: IetfVersion): Promise<SetupOptions> {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({
			write(chunk) {
				chunks.push(new Uint8Array(chunk));
			},
		}),
		version,
	);
	await params.encode(writer);
	writer.close();
	await writer.closed;

	const bytes = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.byteLength, 0));
	let offset = 0;
	for (const chunk of chunks) {
		bytes.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return SetupOptions.decode(new Reader(undefined, bytes, version));
}

/** A raw Token structure: varint fields then a value. */
function raw(fields: bigint[], value: Uint8Array = new Uint8Array()): Uint8Array {
	const parts = [...fields.map((field) => Varint.encodeLeadingOnes(field)), value];
	const out = new Uint8Array(parts.reduce((total, part) => total + part.length, 0));
	let offset = 0;
	for (const part of parts) {
		out.set(part, offset);
		offset += part.length;
	}
	return out;
}

/** The Token structure as a SETUP option. */
function structure(fields: bigint[], value: Uint8Array = new Uint8Array()): SetupOptions {
	const params = new SetupOptions();
	params.setBytes(SetupOption.AuthorizationToken, raw(fields, value));
	return params;
}

function sessionCode(fn: () => unknown): SessionCode | undefined {
	try {
		fn();
	} catch (err) {
		if (err instanceof ProtocolViolation) return err.code;
		throw err;
	}
	return undefined;
}

test("USE_VALUE round trips on every draft", async () => {
	for (const version of VERSIONS) {
		const params = new SetupOptions();
		tokenIntoSetup(params, TOKEN);
		expect(tokenFromSetup(await received(params, version))).toEqual(TOKEN);
	}
});

test("a request carries an out-of-band token by value on every draft", () => {
	const value = new Uint8Array([0x6a, 0x77, 0x74]);
	const params = new Parameters();
	tokenIntoRequest(params, value);
	const raw = params.bytes.get(AUTHORIZATION_TOKEN);
	expect(raw).toBeDefined();
	expect(tokenFromRequest(raw ?? new Uint8Array())).toEqual({ kind: TOKEN_OUT_OF_BAND, value });
});

/** The same bytes `rs/moq-net/src/ietf/token.rs` asserts, so the two agree on the wire. */
test("the encoding matches the cross-language vector", () => {
	const token: Token = { kind: 300n, value: new Uint8Array([0x00, 0xff]) };
	const params = new SetupOptions();
	tokenIntoSetup(params, token);
	expect(params.getBytes(SetupOption.AuthorizationToken)).toEqual(new Uint8Array([0x03, 0x81, 0x2c, 0x00, 0xff]));
});

test("an absent option is no token", () => {
	expect(tokenFromSetup(new SetupOptions())).toBeUndefined();
});

test("an empty value is a token", () => {
	const params = structure([0x3n, TOKEN_OUT_OF_BAND]);
	expect(tokenFromSetup(params)).toEqual({ kind: TOKEN_OUT_OF_BAND, value: new Uint8Array() });
});

/** We advertise no cache, so a registration is the draft's own USE_VALUE. */
test("REGISTER is a value", () => {
	const params = structure([0x1n, 7n, TOKEN.kind], TOKEN.value);
	expect(tokenFromSetup(params)).toEqual(TOKEN);
});

/** Section 9.1.4: nothing can have been registered before SETUP, so an alias there is a violation. */
test("a SETUP alias reference is a protocol violation", () => {
	for (const aliasType of [0x0n, 0x2n]) {
		const params = structure([aliasType, 7n]);
		expect(sessionCode(() => tokenFromSetup(params))).toBe(SessionCode.ProtocolViolation);
	}
});

test("an undecodable structure is a formatting error", () => {
	for (const fields of [[], [0x3n], [0x1n], [0x1n, 7n], [0x0n], [0x2n], [0x4n, 0n]]) {
		const params = structure(fields);
		expect(sessionCode(() => tokenFromSetup(params))).toBe(SessionCode.KeyValueFormatting);
	}
});

/** One credential per connection: a second token is refused, not unioned or dropped. */
test("two tokens are refused", async () => {
	const varint = Varint.encodeLeadingOnes;
	for (const version of VERSIONS) {
		const value = [...varint(0x3n), ...varint(TOKEN_OUT_OF_BAND)];
		// Keys are delta-encoded, so the repeat is a delta of zero.
		const entry = (k: bigint) => [...varint(k), ...varint(BigInt(value.length)), ...value];
		const bytes = new Uint8Array([...entry(SetupOption.AuthorizationToken), ...entry(0n)]);

		await expect(SetupOptions.decode(new Reader(undefined, bytes, version))).rejects.toThrow(/duplicate parameter/);
	}
});

test("a request token by value decodes", () => {
	expect(tokenFromRequest(raw([0x3n, TOKEN.kind], TOKEN.value))).toEqual(TOKEN);
});

/** Only SETUP falls back to a value: on a request, a registration overflows the cache. */
test("a request registration overflows the cache", () => {
	const token = raw([0x1n, 7n, TOKEN.kind], TOKEN.value);
	expect(sessionCode(() => tokenFromRequest(token))).toBe(SessionCode.AuthTokenCacheOverflow);
});

test("a request alias reference is an unknown alias", () => {
	for (const aliasType of [0x0n, 0x2n]) {
		const token = raw([aliasType, 7n]);
		expect(sessionCode(() => tokenFromRequest(token))).toBe(SessionCode.UnknownAuthTokenAlias);
	}
});

test("an undecodable request token is a formatting error", () => {
	// A registration, deletion, or alias use missing a field it names, or an alias use
	// carrying a token type.
	for (const fields of [[], [0x3n], [0x1n], [0x1n, 7n], [0x0n], [0x2n], [0x2n, 7n, 0n], [0x4n, 0n]]) {
		const token = raw(fields);
		expect(sessionCode(() => tokenFromRequest(token))).toBe(SessionCode.KeyValueFormatting);
	}
});
