/**
 * A typed failure code from draft-lcurley-moq-sign.
 *
 * - `signature`: a frame is shorter than a signature, or its signature does not verify at its identity.
 * - `epoch`: the broadcast was not resolved through a route that names its publisher epoch.
 * - `key`: a public key is not 32 bytes, or its text is not 64 lowercase hex characters.
 */
export type Code = "signature" | "epoch" | "key";

/** A profile failure; the message is the {@link Code}. */
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
