import { SetupOption, type SetupOptions } from "./parameters.ts";

/**
 * The MoQ Hidden extension (draft-lcurley-moq-hidden-00).
 *
 * A namespace with a field starting with `.` below the prefix a subscription asked for is
 * left out of discovery unless the SUBSCRIBE_NAMESPACE opts in with the HIDDEN parameter.
 * An unknown parameter fails decoding, so the parameter is only sent to a peer whose SETUP
 * carried the HIDDEN option.
 *
 * @module
 * @internal
 */

/**
 * Whether the peer understands the HIDDEN parameter.
 *
 * @internal
 */
export function hiddenFromSetup(params: SetupOptions): boolean {
	return params.getVarint(SetupOption.Hidden) !== undefined;
}

/**
 * Declare that we understand the HIDDEN parameter.
 *
 * @internal
 */
export function hiddenIntoSetup(params: SetupOptions) {
	params.setVarint(SetupOption.Hidden, 1n);
}
