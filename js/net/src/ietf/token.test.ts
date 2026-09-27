import { expect, test } from "bun:test";
import { SessionCode, SessionError } from "../error.ts";
import { Reader, Writer } from "../stream.ts";
import * as Varint from "../varint.ts";
import { SetupOption, SetupOptions } from "./parameters.ts";
import { TOKEN_OUT_OF_BAND, type Token, tokenFromSetup, tokenIntoSetup } from "./token.ts";
import { type IetfVersion, Version } from "./version.ts";

const VERSIONS: IetfVersion[] = [
	Version.DRAFT_14,
	Version.DRAFT_15,
	Version.DRAFT_16,
	Version.DRAFT_17,
	Version.DRAFT_18,
	Version.DRAFT_19,
	Version.DRAFT_20,
	Version.DRAFT_21,
	Version.DRAFT_22,
];

// A kind past one varint byte and a value that is not text, matching the Rust tests.
const TOKEN: Token = { kind: 300n, value: new Uint8Array([0x00, 0xff, 0x03, 0x80, 0x6a]) };

function leadingOnes(version: IetfVersion): boolean {
	return version !== Version.DRAFT_14 && version !== Version.DRAFT_15 && version !== Version.DRAFT_16;
}

function varint(v: bigint, version: IetfVersion): Uint8Array {
	return leadingOnes(version) ? Varint.encodeLeadingOnes(v) : Varint.encode(v);
}

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
	await params.encode(writer, version);
	writer.close();
	await writer.closed;

	const bytes = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.byteLength, 0));
	let offset = 0;
	for (const chunk of chunks) {
		bytes.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return SetupOptions.decode(new Reader(undefined, bytes, version), version);
}

/** A raw Token structure: varint fields then a value. */
function structure(version: IetfVersion, fields: bigint[], value: Uint8Array = new Uint8Array()): SetupOptions {
	const parts = [...fields.map((field) => varint(field, version)), value];
	const raw = new Uint8Array(parts.reduce((total, part) => total + part.length, 0));
	let offset = 0;
	for (const part of parts) {
		raw.set(part, offset);
		offset += part.length;
	}
	const params = new SetupOptions();
	params.setBytes(SetupOption.AuthorizationToken, raw);
	return params;
}

function sessionCode(fn: () => unknown): SessionCode | undefined {
	try {
		fn();
	} catch (err) {
		if (err instanceof SessionError) return err.code;
		throw err;
	}
	return undefined;
}

test("USE_VALUE round trips on every draft", async () => {
	for (const version of VERSIONS) {
		const params = new SetupOptions();
		tokenIntoSetup(params, TOKEN, version);
		expect(tokenFromSetup(await received(params, version), version)).toEqual(TOKEN);
	}
});

/** The same bytes `rs/moq-net/src/ietf/token.rs` asserts, so the two agree on the wire. */
test("the encoding matches the cross-language vector", () => {
	const token: Token = { kind: 300n, value: new Uint8Array([0x00, 0xff]) };
	for (const [version, expected] of [
		[Version.DRAFT_14, [0x03, 0x41, 0x2c, 0x00, 0xff]],
		[Version.DRAFT_17, [0x03, 0x81, 0x2c, 0x00, 0xff]],
	] as const) {
		const params = new SetupOptions();
		tokenIntoSetup(params, token, version);
		expect(params.getBytes(SetupOption.AuthorizationToken)).toEqual(new Uint8Array(expected));
	}
});

test("an absent option is no token", () => {
	for (const version of VERSIONS) {
		expect(tokenFromSetup(new SetupOptions(), version)).toBeUndefined();
	}
});

test("an empty value is a token", () => {
	for (const version of VERSIONS) {
		const params = structure(version, [0x3n, TOKEN_OUT_OF_BAND]);
		expect(tokenFromSetup(params, version)).toEqual({ kind: TOKEN_OUT_OF_BAND, value: new Uint8Array() });
	}
});

/** We advertise no cache, so a registration is the draft's own USE_VALUE. */
test("REGISTER is a value", () => {
	for (const version of VERSIONS) {
		const params = structure(version, [0x1n, 7n, TOKEN.kind], TOKEN.value);
		expect(tokenFromSetup(params, version)).toEqual(TOKEN);
	}
});

test("an alias reference is a protocol violation", () => {
	for (const version of VERSIONS) {
		for (const aliasType of [0x0n, 0x2n]) {
			const params = structure(version, [aliasType, 7n]);
			expect(sessionCode(() => tokenFromSetup(params, version))).toBe(SessionCode.ProtocolViolation);
		}
	}
});

test("an undecodable structure is a formatting error", () => {
	for (const version of VERSIONS) {
		for (const fields of [[], [0x3n], [0x1n], [0x1n, 7n], [0x4n, 0n]]) {
			const params = structure(version, fields);
			expect(sessionCode(() => tokenFromSetup(params, version))).toBe(SessionCode.KeyValueFormatting);
		}
	}
});

/** One credential per connection: a second token is refused, not unioned or dropped. */
test("two tokens are refused", async () => {
	for (const version of VERSIONS) {
		const value = [...varint(0x3n, version), ...varint(TOKEN_OUT_OF_BAND, version)];
		const key = SetupOption.AuthorizationToken;
		// Delta-encoded from draft-16, so the repeat is a delta of zero.
		const repeat = version === Version.DRAFT_14 || version === Version.DRAFT_15 ? key : 0n;
		const count = leadingOnes(version) ? [] : [...varint(2n, version)];
		const entry = (k: bigint) => [...varint(k, version), ...varint(BigInt(value.length), version), ...value];
		const bytes = new Uint8Array([...count, ...entry(key), ...entry(repeat)]);

		await expect(SetupOptions.decode(new Reader(undefined, bytes, version), version)).rejects.toThrow(
			/duplicate parameter/,
		);
	}
});
