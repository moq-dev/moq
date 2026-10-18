/**
 * A typed failure code from draft-lcurley-moq-e2ee. `duplicate` is a datagram event, never thrown.
 *
 * - `invalid_secret`: the secret is not 32 bytes, or not a nonextractable HKDF key with `deriveBits`.
 * - `identity`: an integer is outside the profile bounds, a `bytes` field exceeds 65535, an epoch is
 *   not canonical UUIDv7 text, or a name is not 22 base64url characters.
 * - `exhausted`: the next AEAD operation would exceed `2^24` uses of that key or `2^36` plaintext bytes.
 * - `reuse`: encrypting at a sequence this generation already allocated, or producing a track it already claimed.
 * - `oversize`: plaintext plus tag exceeds the transport payload limit, or a ciphertext is shorter
 *   than the tag or larger than that limit.
 * - `authentication`: AEAD open failed, including relocation across any identity field.
 */
export type Code = "invalid_secret" | "identity" | "exhausted" | "reuse" | "oversize" | "authentication";

/** A profile failure; the message is the {@link Code}, and never carries secrets, keys, or plaintext. */
export class Failure extends Error {
	/** The draft's typed failure code. */
	readonly code: Code;

	/** Build a failure for `code`. */
	constructor(code: Code) {
		super(code);
		this.name = "Failure";
		this.code = code;
	}
}
