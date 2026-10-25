import { ProtocolViolation, SessionCode } from "../error.ts";
import * as Varint from "../varint.ts";
import type { Parameters } from "./parameters.ts";
import type { SetupOptions } from "./setup_options.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * The `AUTHORIZATION TOKEN` Setup Option (draft-ietf-moq-transport-22 section 9.1.4) and
 * Message Parameter.
 *
 * The value is the Token structure of section 8.9: an Alias Type, then fields that type
 * selects. We advertise no `MAX_AUTH_TOKEN_CACHE_SIZE`, so its default of 0 means no alias
 * is ever registered and every token arrives by value.
 *
 * Mirrors `rs/moq-net/src/ietf/token.rs`.
 *
 * @module
 * @internal
 */

/**
 * The `AUTHORIZATION TOKEN` key, the same as a Setup Option and as a Message Parameter.
 *
 * @internal
 */
export const AUTHORIZATION_TOKEN = 0x03n;

/** Retire a registered alias. */
const DELETE = 0x0n;
/** Register an alias for this type and value, then use them. */
const REGISTER = 0x1n;
/** Use the type and value a registered alias names. */
const USE_ALIAS = 0x2n;
/** Use the type and value carried inline. */
const USE_VALUE = 0x3n;

/**
 * A credential presented in an `AUTHORIZATION TOKEN` option or parameter.
 *
 * @internal
 */
export interface Token {
	/** The wire Token Type, naming how `value` is encoded. */
	kind: bigint;
	/** The token itself. */
	value: Uint8Array;
}

/**
 * Token Type 0: a format the endpoints agreed on out of band, such as a JWT.
 *
 * @internal
 */
export const TOKEN_OUT_OF_BAND = 0x0n;

/** Draft-17 replaced QUIC's two-bit-length varint with a leading-ones one. */
function leadingOnes(version: IetfVersion): boolean {
	return version !== Version.DRAFT_14 && version !== Version.DRAFT_15 && version !== Version.DRAFT_16;
}

/**
 * The token the peer's SETUP presented, if any.
 *
 * A second token is already refused as a duplicate option by {@link SetupOptions}: one
 * credential per connection.
 *
 * @throws {ProtocolViolation} carrying the session code to close with.
 * @internal
 */
export function tokenFromSetup(params: SetupOptions, version: IetfVersion): Token | undefined {
	const raw = params.getBytes(AUTHORIZATION_TOKEN);
	if (raw === undefined) return undefined;
	return decode(raw, version, true);
}

/**
 * A request whose tokens repeat one another, which section 8.9 rejects with
 * MALFORMED_AUTH_TOKEN rather than closing the session.
 *
 * @internal
 */
export class MalformedAuthToken extends Error {
	/** The refused request. Every request starts with its Request ID, so framing fills it in. */
	requestId?: bigint;

	constructor() {
		super("repeated AUTHORIZATION TOKEN");
		this.name = "MalformedAuthToken";
	}
}

/**
 * Refuse a request whose tokens repeat one another.
 *
 * Section 8.9 lets a token repeat only with a different type or value, after resolving
 * aliases, of which we accept none. Checked once every parameter decoded, so a token that
 * closes the session wins over a repeat.
 *
 * @throws {MalformedAuthToken}
 * @internal
 */
export function unique(tokens: Token[]) {
	const same = (a: Token, b: Token) =>
		a.kind === b.kind && a.value.length === b.value.length && a.value.every((byte, i) => byte === b.value[i]);
	for (let i = 0; i < tokens.length; i++) {
		if (tokens.slice(0, i).some((prev) => same(prev, tokens[i]))) throw new MalformedAuthToken();
	}
}

/**
 * Decode an `AUTHORIZATION TOKEN` message parameter on a request, by the SETUP option's rules
 * except that a registration overflows the cache instead of falling back to a value.
 *
 * @throws {ProtocolViolation} carrying the session code to close with.
 * @internal
 */
export function tokenFromRequest(raw: Uint8Array, version: IetfVersion): Token {
	return decode(raw, version, false);
}

/**
 * Decode a Token structure, refusing what this endpoint cannot accept.
 *
 * @throws {ProtocolViolation} `AuthTokenCacheOverflow` for a registration outside SETUP,
 * `UnknownAuthTokenAlias` for an alias reference on a request (`ProtocolViolation` in SETUP),
 * or `KeyValueFormatting` for a structure that cannot be decoded.
 */
function decode(raw: Uint8Array, version: IetfVersion, setup: boolean): Token {
	// Section 8.9: a structure that cannot be decoded closes with KEY_VALUE_FORMATTING_ERROR.
	const malformed = (cause?: unknown) =>
		new ProtocolViolation("malformed AUTHORIZATION TOKEN", { cause, code: SessionCode.KeyValueFormatting });
	const unvarint = (buf: Uint8Array): [bigint, Uint8Array] => {
		try {
			return leadingOnes(version) ? Varint.decodeLeadingOnes(buf) : Varint.decodeBigInt(buf);
		} catch (err) {
			throw malformed(err);
		}
	};

	// Every field an alias type names decodes before its meaning applies, so a truncated
	// structure is malformed whatever its type.
	let [aliasType, rest] = unvarint(raw);
	switch (aliasType) {
		case USE_VALUE:
			break;
		case REGISTER:
			[, rest] = unvarint(rest);
			// Section 8.9: a registration past the cache size of 0 closes the session. With no
			// cache, section 9.1.4 treats one in SETUP as a value and the alias is unused.
			if (!setup) {
				unvarint(rest);
				throw new ProtocolViolation("AUTHORIZATION TOKEN registration", {
					code: SessionCode.AuthTokenCacheOverflow,
				});
			}
			break;
		case DELETE:
		case USE_ALIAS: {
			const [, trailing] = unvarint(rest);
			if (trailing.length > 0) throw malformed();
			// Section 9.1.4: nothing can have been registered before SETUP.
			if (setup) throw new ProtocolViolation("AUTHORIZATION TOKEN alias in SETUP");
			// Section 9.1.3: a cache size of 0 prohibits aliases, so none was ever registered.
			// Section 8.9 rejects the message, but 0x17 is only a session code (section 12.2),
			// so the session closes; doc/concept/standard.md records the deviation.
			throw new ProtocolViolation("unknown AUTHORIZATION TOKEN alias", {
				code: SessionCode.UnknownAuthTokenAlias,
			});
		}
		default:
			throw malformed();
	}

	const [kind, value] = unvarint(rest);
	return { kind, value: value.slice() };
}

/**
 * Present `token` in our SETUP, by value.
 *
 * @internal
 */
export function tokenIntoSetup(params: SetupOptions, token: Token, version: IetfVersion) {
	params.setBytes(AUTHORIZATION_TOKEN, encode(token, version));
}

/**
 * Carry `value`, a token in a format agreed out of band, on a request, by value.
 *
 * @internal
 */
export function tokenIntoRequest(params: Parameters, value: Uint8Array, version: IetfVersion) {
	params.bytes.set(AUTHORIZATION_TOKEN, encode({ kind: TOKEN_OUT_OF_BAND, value }, version));
}

/** A Token structure by value: USE_VALUE, its type, then the token. */
function encode(token: Token, version: IetfVersion): Uint8Array {
	const varint = (v: bigint) => (leadingOnes(version) ? Varint.encodeLeadingOnes(v) : Varint.encode(v));
	const aliasType = varint(USE_VALUE);
	const kind = varint(token.kind);

	const out = new Uint8Array(aliasType.length + kind.length + token.value.length);
	out.set(aliasType, 0);
	out.set(kind, aliasType.length);
	out.set(token.value, aliasType.length + kind.length);
	return out;
}
