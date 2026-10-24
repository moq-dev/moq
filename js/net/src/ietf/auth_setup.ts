import { SetupOption, type SetupOptions } from "./setup_options.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * Negotiation of the MoQ Auth extension (draft-lcurley-moq-auth-01) with the AUTH Setup Option.
 * Kept apart from its request streams in `auth.ts`, which only an established session loads.
 *
 * @module
 * @internal
 */

/** Whether a version negotiates the extension. @internal */
export function supported(version: IetfVersion): boolean {
	return version >= Version.DRAFT_17;
}

/**
 * What the peer declared: undefined for no option, otherwise whether it offered the
 * extension. Only an explicit 1 negotiates it.
 *
 * @internal
 */
export function fromSetup(params: SetupOptions, version: IetfVersion): boolean | undefined {
	if (!supported(version)) return undefined;
	const value = params.getVarint(SetupOption.Auth);
	return value === undefined ? undefined : value === 1n;
}

/** Offer the extension, on the versions that negotiate it. @internal */
export function intoSetup(params: SetupOptions, version: IetfVersion) {
	if (supported(version)) params.setVarint(SetupOption.Auth, 1n);
}
