import { ProtocolViolation, SessionCode } from "../error.ts";
import * as Varint from "../varint.ts";
import type { SetupOptions } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * The `AUTHORIZATION TOKEN` Setup Option (draft-ietf-moq-transport-21 section 9.1.4) and
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

/**
 * Token Type 1: a Common Access Token (draft-ietf-moq-c4m).
 *
 * @internal
 */
export const TOKEN_CAT = 0x1n;

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
 * `UnknownAuthTokenAlias` for an alias reference, or `KeyValueFormatting` for a structure that
 * cannot be decoded.
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

	let [aliasType, rest] = unvarint(raw);
	switch (aliasType) {
		case USE_VALUE:
			break;
		case REGISTER:
			// Section 8.9: a registration past the cache size of 0 closes the session.
			if (!setup) {
				throw new ProtocolViolation("AUTHORIZATION TOKEN registration", {
					code: SessionCode.AuthTokenCacheOverflow,
				});
			}
			// With no cache, section 9.1.4 treats a registration in SETUP as a value; the alias
			// is unused.
			[, rest] = unvarint(rest);
			break;
		// Section 9.1.3: a cache size of 0 prohibits aliases, so none was ever registered.
		// Section 8.9 rejects the message, but 0x17 is only a session code (section 12.2), so
		// the session closes; doc/concept/standard.md records the deviation.
		case DELETE:
		case USE_ALIAS:
			throw new ProtocolViolation("unknown AUTHORIZATION TOKEN alias", {
				code: SessionCode.UnknownAuthTokenAlias,
			});
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
	const varint = (v: bigint) => (leadingOnes(version) ? Varint.encodeLeadingOnes(v) : Varint.encode(v));
	const aliasType = varint(USE_VALUE);
	const kind = varint(token.kind);

	const out = new Uint8Array(aliasType.length + kind.length + token.value.length);
	out.set(aliasType, 0);
	out.set(kind, aliasType.length);
	out.set(token.value, aliasType.length + kind.length);
	params.setBytes(AUTHORIZATION_TOKEN, out);
}
