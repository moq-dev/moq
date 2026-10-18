import { SetupOption, type SetupOptions } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

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

// Draft-14/15 answer SUBSCRIBE_NAMESPACE with PUBLISH_NAMESPACE requests, which the stream cannot count.
function supported(version: IetfVersion): boolean {
	return version !== Version.DRAFT_14 && version !== Version.DRAFT_15;
}

/**
 * Whether the extension is negotiated. We always declare it, so the peer's SETUP decides.
 *
 * @internal
 */
export function activeCountFromSetup(params: SetupOptions, version: IetfVersion): boolean {
	return supported(version) && params.getVarint(SetupOption.ActiveCount) !== undefined;
}

/**
 * Declare that we count and read the peer's count.
 *
 * @internal
 */
export function activeCountIntoSetup(params: SetupOptions, version: IetfVersion) {
	if (supported(version)) params.setVarint(SetupOption.ActiveCount, 1n);
}
