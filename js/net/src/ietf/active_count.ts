import { SetupOption, type SetupOptions } from "./setup_options.ts";

/**
 * The MoQ Active Count extension (draft-lcurley-moq-active-count-00).
 *
 * A publisher answers SUBSCRIBE_NAMESPACE with a NAMESPACE for every suffix it already
 * advertises, then reports changes on the same stream. Nothing in moq-transport says where
 * the first part ends, so when both endpoints declare the ACTIVE_COUNT option, the
 * publisher puts on its REQUEST_OK how many NAMESPACE messages come before the
 * subscription is synced.
 *
 * @module
 * @internal
 */

/**
 * Whether the extension is negotiated. We always declare it, so the peer's SETUP decides.
 *
 * @internal
 */
export function activeCountFromSetup(params: SetupOptions): boolean {
	return params.getVarint(SetupOption.ActiveCount) !== undefined;
}

/**
 * Declare that we count and read the peer's count.
 *
 * @internal
 */
export function activeCountIntoSetup(params: SetupOptions) {
	params.setVarint(SetupOption.ActiveCount, 1n);
}
