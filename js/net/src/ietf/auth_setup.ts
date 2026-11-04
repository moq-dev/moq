import { SetupOption, type SetupOptions } from "./setup_options.ts";

/**
 * Negotiation of the MoQ Auth extension (draft-lcurley-moq-auth-01) with the AUTH Setup Option.
 * Kept apart from its request streams in `auth.ts`, which only an established session loads.
 *
 * @module
 * @internal
 */

/**
 * What the peer declared: undefined for no option, otherwise whether it offered the
 * extension. Only an explicit 1 negotiates it.
 *
 * @internal
 */
export function fromSetup(params: SetupOptions): boolean | undefined {
	const value = params.getVarint(SetupOption.Auth);
	return value === undefined ? undefined : value === 1n;
}

/** Offer the extension. @internal */
export function intoSetup(params: SetupOptions) {
	params.setVarint(SetupOption.Auth, 1n);
}
