import { SetupOption, type SetupOptions } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * The MoQ Epoch extension (draft-lcurley-moq-epoch-00).
 *
 * moq-transport names content by namespace alone, so a publisher that restarts at the same
 * namespace looks like the same broadcast to every relay. This extension carries the
 * publisher instance, the epoch moq-lite 07 puts on its routes and requests, as an EPOCH
 * parameter: on each advertisement (PUBLISH_NAMESPACE and NAMESPACE), and on each request
 * that resolves one (SUBSCRIBE, FETCH, and TRACK_STATUS). A namespace and an EPOCH name one
 * broadcast; a request carrying one is refused rather than served by another instance.
 *
 * Negotiated on draft-17+ only, where SETUP is a Key-Value-Pair block, and only when both
 * endpoints declare the option.
 *
 * @module
 * @internal
 */

/** Whether a version negotiates the extension: draft-17+, like MoQ Cluster. @internal */
export function supported(version: IetfVersion): boolean {
	return version !== Version.DRAFT_14 && version !== Version.DRAFT_15 && version !== Version.DRAFT_16;
}

/** Whether the peer declared the option. @internal */
export function fromSetup(params: SetupOptions, version: IetfVersion): boolean {
	return supported(version) && params.getVarint(SetupOption.Epoch) !== undefined;
}

/** Declare that we understand the EPOCH parameter, on the versions that negotiate it. @internal */
export function intoSetup(params: SetupOptions, version: IetfVersion) {
	if (supported(version)) params.setVarint(SetupOption.Epoch, 1n);
}
