import { SessionCode, SessionError } from "../error.ts";
import * as Varint from "../varint.ts";
import { SetupOption, type SetupOptions } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * The `AUTHORIZATION TOKEN` Setup Option (draft-ietf-moq-transport-21 section 9.1.4).
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

/** Retire a registered alias. */
const DELETE = 0x0n;
/** Register an alias for this type and value, then use them. */
const REGISTER = 0x1n;
/** Use the type and value a registered alias names. */
const USE_ALIAS = 0x2n;
/** Use the type and value carried inline. */
const USE_VALUE = 0x3n;

/**
 * A credential presented in a SETUP's `AUTHORIZATION TOKEN` option.
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
 * @throws {SessionError} `ProtocolViolation` for an alias reference, which nothing before
 * SETUP could have registered, or `KeyValueFormatting` for a structure that cannot be decoded.
 * @internal
 */
export function tokenFromSetup(params: SetupOptions, version: IetfVersion): Token | undefined {
	const raw = params.getBytes(SetupOption.AuthorizationToken);
	if (raw === undefined) return undefined;

	// Section 8.9: a structure that cannot be decoded closes with KEY_VALUE_FORMATTING_ERROR.
	const malformed = (cause?: unknown) =>
		new SessionError(SessionCode.KeyValueFormatting, { cause, reason: "malformed AUTHORIZATION TOKEN" });
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
		// With no cache, section 9.1.4 treats a registration as a value; the alias is unused.
		case REGISTER:
			[, rest] = unvarint(rest);
			break;
		// Section 9.1.4: nothing can have been registered before SETUP.
		case DELETE:
		case USE_ALIAS:
			throw new SessionError(SessionCode.ProtocolViolation, { reason: "AUTHORIZATION TOKEN alias in SETUP" });
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
	params.setBytes(SetupOption.AuthorizationToken, out);
}
