/** Typed failure from the moq-e2ee-01 profile. */
export type Code =
	| "unsupported_profile"
	| "invalid_secret"
	| "identity"
	| "exhausted"
	| "reuse"
	| "oversize"
	| "authentication"
	| "duplicate"
	| "pinned_mismatch";

/**
 * A profile failure. `code` is the stable wire/API token; `message` never includes
 * secrets, keys, or key material.
 */
export class Failure extends Error {
	/** The typed profile failure. */
	readonly code: Code;

	/** Create a failure with a profile {@link Code}. */
	constructor(code: Code, message?: string) {
		super(message ?? code);
		this.name = "Failure";
		this.code = code;
	}
}

/** True when `value` is a {@link Failure}. */
export function isFailure(value: unknown): value is Failure {
	return value instanceof Failure;
}
